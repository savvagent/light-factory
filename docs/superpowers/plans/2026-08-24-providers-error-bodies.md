# Provider Error Bodies On The Model-List Path — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a non-2xx model-list response, capture a bounded, sanitized, key-redacted snippet of
the provider's own error body and carry it into the `anyhow` error — so a Gemini user with a bad key
sees "API key not valid. Please pass a valid API key." instead of a bare `400 Bad Request` — while
keeping the underlying `reqwest::Error` as the error's source so the TUI's existing status-based
classification is untouched.

**Architecture:** `parse_capped`'s opening `raw.error_for_status()?` is replaced by a new private
`check_status`, which uses `error_for_status_ref` (borrowing rather than consuming, so the body is
still readable), reads a bounded prefix of the body through the module's existing `read_capped`
under a new `ListBounds::max_error_bytes`, extracts the provider's diagnostic from the two JSON
envelope shapes all five providers use, normalizes it to one control-free line, redacts the API key
out of it, caps it at 200 characters, and attaches it as `anyhow` **context** on top of the
`reqwest::Error` — which stays the **source**. Everything is private to `crates/providers`.

**Tech Stack:** Rust (edition 2024, toolchain pinned in `rust-toolchain.toml`); existing `reqwest`
0.13, `anyhow`, `serde`/`serde_json`; existing `wiremock` 0.6 dev-dependency. No new dependency.

**Spec:** `docs/superpowers/specs/2026-08-24-providers-error-bodies-design.md` — read it first. This
plan implements it exactly.

## Global Constraints

- **Do not touch `crates/tui`.** PR #68 (`tui-keyring-failure-vs-miss`) is in flight and rewrites
  `crates/tui/src/{selection,modal,app,provider,i18n,text}.rs`. The whole diff of this plan is one
  file: `crates/providers/src/models.rs`. In particular, `class_for_status(Some(400)) == Fetch` in
  `crates/tui/src/modal.rs` stays exactly as it is — the classification rule does **not** change.
- Inward dependency flow: `crates/providers` must not gain a dependency on `crates/tui` or on any
  app crate. No new entry in `crates/providers/Cargo.toml`.
- **Secrets never reach a log line or an error message.** The API key is redacted out of every
  captured error body before it can be truncated. No `println!`/`tracing` call is added.
- **No test may reach the network.** Every new test runs against a local `wiremock::MockServer`.
- **No AI / Co-Authored-By / "Generated with" attribution** in commits, PR bodies, code comments, or
  docs.
- Comments follow the surrounding style of `crates/providers/src/models.rs`: this module documents
  *why* a bound or an ordering is load-bearing, in full sentences, on every non-obvious function.
  Match that density — it is what the existing reviewers of this file expect.
- Semver: every new item is private (`ListBounds` is `pub(crate)`; all new functions and types are
  module-private). `list_models`, `list_ollama_models`, `list_models_at`, `list_ollama_models_at`
  keep their exact signatures ⇒ **additive / semver-minor ⇒ no `Cargo.toml` version bump**
  (Non-Negotiable Rule 6). Note `crates/providers` uses `version.workspace = true`, so a bump would
  move every workspace crate.
- Run `cargo fmt --all` before every Rust commit. Lint with
  `cargo clippy --workspace --all-targets -- -D warnings`.
- Tests live next to the code in the existing `#[cfg(test)] mod tests` at the bottom of
  `crates/providers/src/models.rs`.
- Out-of-band surfaces (`Dockerfile`, `fly.toml`, `web/`, `crates/persistence/migrations/`,
  `.github/`) are **not** touched by this plan — Phase 5 verification is vacuous for all of them.

## File Structure

| File | Responsibility |
|---|---|
| Modify. `crates/providers/src/models.rs` | `ListBounds::max_error_bytes` (field, `DEFAULT` value, `debug_check` assertion); `DETAIL_MAX_CHARS`; `check_status`, `error_detail`, `extract_error_message`, `normalize_detail`, `redact_secret`, `cap_chars`; `ErrorEnvelope` + `ErrorPayload`; `parse_capped` gains a `secret: Option<&str>` parameter; its four call sites; the `tight()` test helper; ten new tests and one extended test |

## Task Order & Rationale

**Single task.** The new `ListBounds` field, the capture machinery, and the `parse_capped` signature
change are one cohesive edit: splitting them would leave a struct field unread or a private function
uncalled between commits, which `clippy -D warnings` rejects as dead code, and would leave the
module in a state where no test can meaningfully assert anything. The whole change is one source
file, TDD-ordered inside the task.

---

### Task 1: capture the provider's error body on a non-2xx model-list response

**Files:**
- Modify: `crates/providers/src/models.rs`
- Test: `crates/providers/src/models.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `read_capped(resp: reqwest::Response, max_bytes: usize) -> anyhow::Result<Vec<u8>>`
  (already in this module, unchanged); `ListBounds` (already in this module).
- Produces: nothing outside the module. `parse_capped` becomes
  `async fn parse_capped<T: serde::de::DeserializeOwned>(raw: reqwest::Response, bounds: ListBounds, secret: Option<&str>) -> anyhow::Result<T>`.

- [ ] **Step 1: Write the failing tests**

Add these to the bottom of `crates/providers/src/models.rs`'s `#[cfg(test)] mod tests`, and update
the existing `tight()` helper and `default_bounds_are_the_production_values` test in place.

Replace the existing `tight()` helper with:

```rust
    /// Bounds tight enough for a test to reach in milliseconds, so every bound below is exercised
    /// deliberately rather than by accident of the production values.
    fn tight() -> ListBounds {
        ListBounds {
            timeout: Duration::from_secs(5),
            max_body_bytes: 64 * 1024,
            max_error_bytes: 16 * 1024,
            max_models: 1_000,
        }
    }
```

Replace the body of the existing `default_bounds_are_the_production_values` test with:

```rust
    #[test]
    fn default_bounds_are_the_production_values() {
        // Pinned so a later accidental widening is a failing test rather than a silent regression.
        assert_eq!(ListBounds::DEFAULT.timeout, Duration::from_secs(15));
        assert_eq!(ListBounds::DEFAULT.max_body_bytes, 2 * 1024 * 1024);
        assert_eq!(ListBounds::DEFAULT.max_error_bytes, 16 * 1024);
        assert_eq!(ListBounds::DEFAULT.max_models, 1_000);
    }
```

Append the new tests:

```rust
    #[tokio::test]
    async fn a_gemini_invalid_key_400_surfaces_the_providers_own_message() {
        // The motivating case for #59. Google answers an invalid API key with 400
        // INVALID_ARGUMENT, not 401/403, so the status line alone tells the user nothing about
        // their key — the body is the only place the diagnosis exists.
        const MESSAGE: &str = "API key not valid. Please pass a valid API key.";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": { "code": 400, "message": MESSAGE, "status": "INVALID_ARGUMENT" }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("gemini", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 400 must be an error");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(MESSAGE),
            "the provider's own diagnostic must reach the error: {chain}"
        );
        assert!(chain.contains("400"), "the status must survive too: {chain}");
    }

    #[tokio::test]
    async fn the_status_error_survives_in_the_chain_for_classification() {
        // The regression guard for the whole design. `crates/tui` recovers the HTTP status by
        // walking `anyhow::Error::chain()` for a `reqwest::Error` and calling `status()`; if the
        // non-2xx arm ever stopped putting that error in the chain, every 401/403 would silently
        // degrade to the retryable class and the credentials step would become unreachable. This
        // test mirrors that walk exactly, so the regression fails here instead of in the TUI.
        const MESSAGE: &str = "Incorrect API key provided";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": MESSAGE, "type": "invalid_request_error", "code": "invalid_api_key" }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 401 must be an error");

        let status = err
            .chain()
            .find_map(|e| e.downcast_ref::<reqwest::Error>())
            .and_then(reqwest::Error::status)
            .map(|s| s.as_u16());
        assert_eq!(
            status,
            Some(401),
            "the status must stay recoverable from the chain: {err:#}"
        );
        assert!(
            format!("{err:#}").contains(MESSAGE),
            "the OpenAI-shaped envelope must be extracted: {err:#}"
        );
    }

    #[tokio::test]
    async fn an_anthropic_error_envelope_is_extracted_despite_its_outer_type_field() {
        // Anthropic wraps the same `error.message` in an envelope with a sibling `type` field.
        // Unknown top-level fields must be ignored rather than failing the parse into the raw-text
        // fallback.
        const MESSAGE: &str = "invalid x-api-key";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "type": "error",
                "error": { "type": "authentication_error", "message": MESSAGE }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("anthropic", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 401 must be an error");
        assert!(
            format!("{err:#}").contains(MESSAGE),
            "the Anthropic-shaped envelope must be extracted: {err:#}"
        );
    }

    #[tokio::test]
    async fn an_ollama_string_error_envelope_is_extracted() {
        // Ollama answers `{"error":"..."}` — a bare string where the others nest an object. It also
        // takes no credential, so this is the one capture path an unauthenticated local process can
        // drive.
        const MESSAGE: &str = "model not found";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({ "error": MESSAGE })),
            )
            .mount(&server)
            .await;

        let err = list_ollama_models_at(&server.uri(), tight())
            .await
            .expect_err("a 404 must be an error");
        assert!(
            format!("{err:#}").contains(MESSAGE),
            "the Ollama-shaped envelope must be extracted: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_non_json_error_body_falls_back_to_its_first_line() {
        // Gateways and proxies answer with HTML or text/plain, and the first line is often the only
        // signal there is. Later lines are dropped: a multi-line error pushes the modal's own
        // trusted rows off the screen.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string("upstream connect error\n<html>502 Bad Gateway</html>"),
            )
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "test-key", tight())
            .await
            .expect_err("a 502 must be an error");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("upstream connect error"),
            "the first line of a non-JSON body must survive: {chain}"
        );
        assert!(
            !chain.contains("Bad Gateway"),
            "only the first line may survive: {chain}"
        );
    }

    #[tokio::test]
    async fn an_empty_error_body_yields_a_status_only_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(chain.contains("401"), "the status must be named: {chain}");
        assert!(
            chain.contains("no error detail"),
            "the absence of a body must be stated, not implied: {chain}"
        );
    }

    #[tokio::test]
    async fn an_oversized_error_body_is_refused_rather_than_captured() {
        // The error path is reached *without* a valid credential, so it is strictly more exposed
        // than the success path. `read_capped` refuses rather than truncates, so an over-cap error
        // body degrades to the status line — which is exactly the pre-#59 behaviour, and strictly
        // better than an attacker-sized allocation.
        const MARKER: &str = "RUN-THIS-COMMAND";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": format!("{MARKER}{}", "x".repeat(4096)) }
            })))
            .mount(&server)
            .await;

        let err = list_models_at(
            "openai",
            &server.uri(),
            "bad-key",
            ListBounds {
                max_error_bytes: 64,
                ..tight()
            },
        )
        .await
        .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains(MARKER),
            "no over-cap body content may reach the error: {chain}"
        );
        assert!(chain.contains("401"), "the status must survive: {chain}");
    }

    #[tokio::test]
    async fn an_error_body_echoing_the_api_key_is_redacted() {
        // A misconfigured gateway — or an attacker-chosen `*_BASE_URL` override — can echo the
        // submitted key back in its error body. Rendering that into a terminal cell would put the
        // user's live credential into their scrollback.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": format!("The API key {KEY} is not authorized") }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), KEY, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(!chain.contains(KEY), "the key must not reach the error: {chain}");
        assert!(
            !chain.contains("0123456789abcdef"),
            "not even a prefix of the key may survive: {chain}"
        );
        assert!(
            chain.contains("<redacted>"),
            "the redaction must be visible rather than silent: {chain}"
        );
    }

    #[tokio::test]
    async fn control_characters_and_extra_lines_are_stripped_from_the_error_detail() {
        // A raw ESC written into a terminal cell is an escape-sequence injection; the body is
        // remote-controlled, so it can carry one.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "message": "denied\u{1b}]0;pwned\u{7}\nsecond line" }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 403 must be an error");

        let chain = format!("{err:#}");
        assert!(chain.contains("denied"), "the diagnostic must survive: {chain}");
        assert!(
            !chain.chars().any(char::is_control),
            "no control character may reach a rendered line: {chain:?}"
        );
        assert!(
            !chain.contains("second line"),
            "only the first line may survive: {chain}"
        );
    }

    #[tokio::test]
    async fn an_over_long_error_detail_is_capped() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": { "message": "z".repeat(1_000) }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 400 must be an error");

        let chain = format!("{err:#}");
        assert!(
            chain.matches('z').count() <= DETAIL_MAX_CHARS,
            "the detail must be capped at {DETAIL_MAX_CHARS} characters: {chain}"
        );
        assert!(
            chain.contains('\u{2026}'),
            "a truncated detail must say so: {chain}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-providers models::tests 2>&1 | tail -40`

Expected: a **compile** failure — `ListBounds` has no field `max_error_bytes`, `DETAIL_MAX_CHARS` is
not defined. That is the correct first failure for a change whose first move is adding a bound.

- [ ] **Step 3: Add the `max_error_bytes` bound**

In `crates/providers/src/models.rs`, add the field to `ListBounds` (after `max_body_bytes`):

```rust
    /// Hard ceiling on buffered bytes when capturing a **non-2xx** response's error body.
    ///
    /// Deliberately far below `max_body_bytes`: an honest provider error body is a JSON envelope of
    /// a few hundred bytes, and nothing above this is a diagnostic. It matters more than the
    /// success cap, not less — the error path is reached *without* a valid credential, so it is the
    /// more exposed of the two. `read_capped` refuses rather than truncates, so an over-cap error
    /// body simply yields no detail and the error degrades to the status line, which is what this
    /// path did before the body was captured at all.
    pub(crate) max_error_bytes: usize,
```

Add its `DEFAULT` value (after `max_body_bytes`'s):

```rust
        // Roughly a hundred times the largest plausible honest error envelope, and small enough
        // that refusing anything larger costs nothing real.
        max_error_bytes: 16 * 1024,
```

Add its assertion to `debug_check`, after the `max_body_bytes` one:

```rust
        debug_assert!(
            self.max_error_bytes >= MIN_BODY_BYTES,
            "an error-body cap below {MIN_BODY_BYTES} bytes cannot admit even a minimal envelope"
        );
```

- [ ] **Step 4: Add the capture machinery**

In `crates/providers/src/models.rs`, immediately after `read_capped`, add:

```rust
/// The longest provider-supplied error detail this crate will carry in an error.
///
/// Set *above* the TUI's own 120-character display cap on purpose. The display boundary decides how
/// much fits on a rendered line; this one is a safety floor for every other consumer of the `pub`
/// entry points — a log line, the server crate, a future non-TUI client — none of which the TUI's
/// cap protects. Two independent bounds is the pattern this module already uses.
const DETAIL_MAX_CHARS: usize = 200;

/// Refuse a non-2xx response, carrying the provider's own diagnostic with it.
///
/// `error_for_status_ref` rather than `error_for_status` is load-bearing: it yields the
/// `reqwest::Error` *without* consuming the response, so the body is still readable. The
/// `reqwest::Error` then becomes the `anyhow` **source** and the captured detail becomes `anyhow`
/// **context** — never the other way round. `crates/tui` recovers the HTTP status by walking
/// `anyhow::Error::chain()` for a `reqwest::Error`, so replacing this with a bare `bail!` would
/// silently degrade every 401/403 to the retryable class and make the credentials step
/// unreachable. `the_status_error_survives_in_the_chain_for_classification` pins that.
///
/// The status is repeated in the context even though `reqwest::Error`'s own Display carries it:
/// the TUI renders only the first 120 characters of `{err:#}`, context first, so without it a long
/// provider message would push the status off the rendered line.
async fn check_status(
    raw: reqwest::Response,
    bounds: ListBounds,
    secret: Option<&str>,
) -> anyhow::Result<reqwest::Response> {
    let Some(status_err) = raw.error_for_status_ref().err() else {
        return Ok(raw);
    };
    let status = status_err.status().map(|s| s.as_u16());
    let detail = error_detail(raw, bounds, secret).await;
    Err(anyhow::Error::new(status_err).context(match (status, detail) {
        (Some(code), Some(detail)) => format!("the provider returned HTTP {code}: {detail}"),
        (Some(code), None) => format!("the provider returned HTTP {code} with no error detail"),
        (None, Some(detail)) => format!("the provider returned an error status: {detail}"),
        (None, None) => "the provider returned an error status".to_string(),
    }))
}

/// Capture one bounded, sanitized, key-redacted line of a non-2xx response body, or `None`.
///
/// `None` on any failure — an over-cap body, a transport error mid-read, a body that normalizes to
/// nothing. Every such case degrades to the status line alone, which is what this path did before
/// the body was captured; nothing about the body reaches that error, which `read_capped` already
/// guarantees.
///
/// The order of the three transforms is deliberate and must not be rearranged:
/// 1. **normalize** first, so a key with interleaved control characters cannot evade the match;
/// 2. **redact** second, before any truncation, so a cut cannot bisect the key and leave a usable
///    prefix;
/// 3. **cap** last.
async fn error_detail(
    resp: reqwest::Response,
    bounds: ListBounds,
    secret: Option<&str>,
) -> Option<String> {
    let body = read_capped(resp, bounds.max_error_bytes).await.ok()?;
    // Lossy rather than `from_utf8().ok()?`: one invalid byte should not discard an otherwise
    // readable diagnostic, and U+FFFD is not a control character.
    let text = String::from_utf8_lossy(&body);
    let extracted = extract_error_message(&text);
    let message = extracted.unwrap_or_else(|| text.into_owned());
    let capped = cap_chars(
        redact_secret(normalize_detail(&message), secret),
        DETAIL_MAX_CHARS,
    );
    (!capped.is_empty()).then_some(capped)
}

/// Pull the provider's own message out of the two envelope shapes every provider on this path uses:
/// `{"error":{"message":"..."}}` (OpenAI, DeepSeek, Anthropic, Gemini) and `{"error":"..."}`
/// (Ollama). Anything else returns `None` and the caller falls back to the raw text.
///
/// That fallback is not a trust downgrade: a `message` field is exactly as sender-chosen as a raw
/// body, so both carry the same trust level and both go through the same normalize/redact/cap.
/// Extracting the envelope is noise reduction, not a boundary.
fn extract_error_message(body: &str) -> Option<String> {
    let envelope: ErrorEnvelope = serde_json::from_str(body).ok()?;
    Some(match envelope.error {
        ErrorPayload::Detailed { message } | ErrorPayload::Text(message) => message,
    })
}

/// First line, control characters removed, trimmed.
///
/// Later lines go because a multi-line error pushes the modal's own trusted rows — the remedy, and
/// on the manual step the input box — past the bottom of the screen, which turns a model picker
/// into a credential-phishing surface. Control characters go because a raw `ESC` written into a
/// terminal cell is an escape-sequence injection, and this text is entirely sender-chosen.
fn normalize_detail(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Replace every occurrence of the API key with a visible marker.
///
/// An endpoint that echoes the submitted key back — a misconfigured gateway, or an attacker-chosen
/// `*_BASE_URL` override — would otherwise put the user's live credential into a terminal cell and
/// into their scrollback. An empty secret is skipped: `str::replace` with an empty pattern matches
/// at every character boundary. The marker is visible rather than a silent deletion so a user who
/// sees it knows why the message reads oddly.
fn redact_secret(text: String, secret: Option<&str>) -> String {
    match secret {
        Some(s) if !s.is_empty() => text.replace(s, "<redacted>"),
        _ => text,
    }
}

/// Truncate to `max` **characters** (not bytes — a byte cut can split a code point) with an
/// explicit ellipsis, so a truncated message never looks like a complete one.
fn cap_chars(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\u{2026}")
}
```

And, next to the other response types near the bottom of the module (just before `#[cfg(test)]`):

```rust
/// The error envelope every provider on this path wraps its diagnostic in. Unknown sibling fields
/// (Anthropic's outer `type`, Gemini's `error.code`/`error.status`) are ignored by serde's default.
#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorPayload,
}

/// Untagged, and the variant order is load-bearing: serde tries variants top to bottom, so the
/// object shape must precede the bare-string shape or `{"message":"..."}` would never match.
#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorPayload {
    Detailed { message: String },
    Text(String),
}
```

- [ ] **Step 5: Route `parse_capped` through `check_status` and thread the secret**

Replace `parse_capped` with:

```rust
/// Parse a 3xx-cleared, status-cleared response's body into `T` under the byte cap.
///
/// Ordering is load-bearing and unchanged from before the cap existed: `reject_redirect` has
/// already refused a 3xx, and [`check_status`] refuses a 4xx/5xx — capturing a bounded snippet of
/// its body — before a single byte of a success body is read. `secret` is the API key this request
/// carried, so it can be redacted out of a body that echoes it; `None` for Ollama, which sends no
/// credential at all.
async fn parse_capped<T: serde::de::DeserializeOwned>(
    raw: reqwest::Response,
    bounds: ListBounds,
    secret: Option<&str>,
) -> anyhow::Result<T> {
    let resp = check_status(raw, bounds, secret).await?;
    let body = read_capped(resp, bounds.max_body_bytes).await?;
    // Line and column only, and deliberately **not** `serde_json::Error`'s own Display: for an
    // `invalid_type` error that Display embeds the offending value verbatim — a body of
    // `{"data":"<attacker text>"}` renders as `invalid type: string "<attacker text>", expected a
    // sequence`. Since this message is interpolated into `connect.fetch_error` and drawn in the
    // modal, keeping it would let an endpoint place up to `max_body_bytes` of chosen text into the
    // TUI framed as this tool's own error — with no credential at all, because the Ollama path
    // targets 127.0.0.1:11434 and any local process squatting that port can drive it.
    serde_json::from_slice(&body).map_err(|e| {
        anyhow::anyhow!(
            "model list response was not valid JSON (line {}, column {})",
            e.line(),
            e.column()
        )
    })
}
```

Update the four call sites:

- in `list_ollama_models_at`: `let resp: OllamaTags = parse_capped(raw, bounds, None).await?;`
- in `list_anthropic`: `let resp: IdList = parse_capped(raw, bounds, Some(key)).await?;`
- in `list_openai_compatible`: `let resp: IdList = parse_capped(raw, bounds, Some(key)).await?;`
- in `list_gemini`: `let resp: GeminiModels = parse_capped(raw, bounds, Some(key)).await?;`

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p light-factory-providers 2>&1 | tail -40`

Expected: PASS — all ten new tests plus every pre-existing test in the crate. In particular
`an_auth_error_is_surfaced_as_an_error` (the outermost message still names 401) and
`a_stalled_endpoint_fails_at_the_deadline` (the timeout still downcasts at the anyhow root, because
it fires at `send()` before any status check) must stay green **unmodified**.

If `check_status`'s `let Some(status_err) = raw.error_for_status_ref().err() else { ... }` fails to
borrow-check, do **not** clone the response or reach for `unsafe`. Rewrite it as:

```rust
    if raw.status().is_success() {
        return Ok(raw);
    }
    let status_err = raw
        .error_for_status_ref()
        .err()
        .expect("a non-success status must produce an error");
```

- [ ] **Step 7: Run the whole workspace suite and the lints**

Run:
```bash
cargo test --workspace 2>&1 | tail -30
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```

Expected: both clean. `crates/tui`'s
`class_for_status_treats_only_401_and_403_as_credential_failures` must be green **and unmodified** —
no classification rule changed.

- [ ] **Step 8: Confirm the diff touches exactly one source file**

Run: `git diff --stat origin/master -- crates/`

Expected: `crates/providers/src/models.rs` only. Any line under `crates/tui` is a scope violation
(PR #68 is in flight there) — revert it.

- [ ] **Step 9: Format and commit**

```bash
cargo fmt --all
git add crates/providers/src/models.rs
git commit -m "providers: carry the provider's error body into model-list failures"
```
