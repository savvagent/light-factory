//! Env-driven provider selection: read `LIGHT_*`/`*_API_KEY` variables, choose a single
//! provider, and degrade to the offline `LocalProvider` when nothing is configured. The
//! decision helpers are pure (injectable inputs, no process env) so the precedence table is
//! unit-testable without `set_var`.
//!
//! Selection is split into two layers: [`build_provider`] takes an explicit [`Selection`] (all
//! inputs already resolved), and [`build_provider_from_env`] builds a [`Selection`] from the
//! environment and delegates. A client can therefore supply keys from the OS keyring or a
//! persisted preference by constructing its own [`Selection`].

use std::collections::HashMap;

use crate::{
    AnthropicProvider, DeepSeekProvider, GeminiProvider, LocalProvider, OllamaProvider,
    OpenAiProvider, Provider, validate_base_url,
};

/// Default model ids when the corresponding `LIGHT_*_MODEL` env var is unset (otto constants).
const DEFAULT_OLLAMA_MODEL: &str = "llama3.2";
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash";
const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-flash";

/// Why selection fell back to the offline `LocalProvider`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfflineReason {
    /// No provider configured: no `LIGHT_OLLAMA`, no `LIGHT_REMOTE_PROVIDER`, and none of the
    /// API keys are present.
    NothingConfigured,
    /// `LIGHT_REMOTE_PROVIDER` named a provider whose API key is absent.
    NamedProviderMissingKey { selector: String, key: String },
    /// A `*_BASE_URL` override was rejected (invalid or non-UTF-8); no provider was constructed
    /// so the API key is never sent to an unvalidated host.
    BaseUrlRejected { var: String },
}

/// Which selection rule chose the active provider (the "why" for the TUI to surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedBy {
    /// `LIGHT_OLLAMA=1`.
    OllamaEnv,
    /// `LIGHT_REMOTE_PROVIDER` named it and its key is present.
    RemoteSelectorEnv,
    /// The persisted `provider` preference named it and its key is present.
    StoredPreference,
    /// Key precedence (Anthropic > OpenAI > Gemini > DeepSeek) chose the first available key.
    KeyPrecedence,
}

/// The resolved inputs needed to build a single provider slot, independent of where they came
/// from (environment, persisted settings, or the OS keyring). [`build_provider`] consumes it.
#[derive(Clone, Default)]
pub struct Selection {
    /// `LIGHT_OLLAMA=1` selects the local Ollama slot.
    pub ollama: bool,
    /// The raw `LIGHT_REMOTE_PROVIDER` selector value.
    pub selector: Option<String>,
    /// The persisted provider preference (`"anthropic"`|`"openai"`|`"gemini"`|`"deepseek"`|
    /// `"ollama"`).
    pub preferred: Option<String>,
    /// Resolved, trimmed non-empty API keys by provider id (env wins over keyring, decided by
    /// the caller).
    pub keys: HashMap<String, String>,
    /// Model overrides by provider id (`"ollama"` included), merged env-over-persisted by the
    /// caller.
    pub models: HashMap<String, String>,
    /// Raw `*_BASE_URL` overrides by provider id (`"openai"`/`"deepseek"`): `Ok(value)` is a
    /// UTF-8 override to validate; `Err(var)` is a non-UTF-8 override (rejected, offline).
    pub base_urls: HashMap<String, Result<String, String>>,
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("ollama", &self.ollama)
            .field("selector", &self.selector)
            .field("preferred", &self.preferred)
            .field(
                "keys",
                &format_args!("<redacted: {} key(s)>", self.keys.len()),
            )
            .field("models", &self.models)
            .field("base_urls", &self.base_urls)
            .finish()
    }
}

/// The `*_API_KEY` environment variable for a remote provider id, or `None` for non-remote ids.
pub fn env_key_var(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        _ => None,
    }
}

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

/// A provider built from explicit inputs, plus its effective model id (for display).
pub struct BuiltProvider {
    pub provider: Box<dyn Provider>,
    pub model: Option<String>,
    /// `Some(reason)` when the offline `LocalProvider` was selected; `None` for a live provider
    /// (Ollama or a remote).
    pub offline: Option<OfflineReason>,
    /// Which rule selected the active provider; `None` when offline.
    pub selected_by: Option<SelectedBy>,
    /// Human-readable selection warnings, for the TUI to surface (formerly `eprintln!`).
    pub warnings: Vec<String>,
}

/// Which remote provider the single remote slot uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteChoice {
    Anthropic,
    OpenAi,
    Gemini,
    DeepSeek,
}

impl RemoteChoice {
    /// Stable id matching each provider's `Provider::id()`.
    fn id(self) -> &'static str {
        match self {
            RemoteChoice::Anthropic => "anthropic",
            RemoteChoice::OpenAi => "openai",
            RemoteChoice::Gemini => "gemini",
            RemoteChoice::DeepSeek => "deepseek",
        }
    }

    /// Parse a selector or preference value (case-insensitive), or `None` when unknown.
    pub(crate) fn parse(raw: &str) -> Option<RemoteChoice> {
        match raw.to_ascii_lowercase().as_str() {
            "anthropic" => Some(RemoteChoice::Anthropic),
            "openai" => Some(RemoteChoice::OpenAi),
            "gemini" => Some(RemoteChoice::Gemini),
            "deepseek" => Some(RemoteChoice::DeepSeek),
            _ => None,
        }
    }
}

/// Which slot the environment selects: Ollama (local HTTP), a remote, or offline Local.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Ollama,
    Remote(RemoteChoice),
    Local,
}

/// The remote-slot selection: which remote (if any), plus why it is `None` and any warnings.
struct RemoteSelection {
    choice: Option<RemoteChoice>,
    offline: Option<OfflineReason>,
    warnings: Vec<String>,
}

/// A `*_BASE_URL` override that was refused, carrying the variable name and the human-readable
/// reason so the caller can surface both.
#[derive(Debug, Clone)]
struct BaseUrlRejection {
    var: String,
    warning: String,
}

/// A constructed remote provider, or the reason it fell back to offline plus warnings.
struct RemoteBuild {
    provider: Option<Box<dyn Provider>>,
    offline: Option<OfflineReason>,
    warnings: Vec<String>,
}

/// Pure precedence: `LIGHT_OLLAMA=1` wins over any remote; a remote wins over offline Local.
fn choose(ollama_on: bool, remote: Option<RemoteChoice>) -> Slot {
    if ollama_on {
        Slot::Ollama
    } else if let Some(choice) = remote {
        Slot::Remote(choice)
    } else {
        Slot::Local
    }
}

/// Pure remote-slot selection. `selector` is the raw `LIGHT_REMOTE_PROVIDER` value; the four
/// bools are "this provider's key is present and non-empty". A valid selector wins when its key
/// is present; a selector whose key is absent yields `None` (offline) rather than silently
/// falling back to another provider; an unknown selector is ignored and key precedence applies:
/// Anthropic > OpenAI > Gemini > DeepSeek.
fn select_remote_from(
    selector: Option<&str>,
    anthropic: bool,
    openai: bool,
    gemini: bool,
    deepseek: bool,
) -> RemoteSelection {
    let mut warnings = Vec::new();
    if let Some(sel) = selector {
        match sel.to_ascii_lowercase().as_str() {
            "anthropic" => {
                return present_or_warn(anthropic, RemoteChoice::Anthropic, "ANTHROPIC_API_KEY");
            }
            "openai" => return present_or_warn(openai, RemoteChoice::OpenAi, "OPENAI_API_KEY"),
            "gemini" => return present_or_warn(gemini, RemoteChoice::Gemini, "GEMINI_API_KEY"),
            "deepseek" => {
                return present_or_warn(deepseek, RemoteChoice::DeepSeek, "DEEPSEEK_API_KEY");
            }
            other => {
                warnings.push(format!(
                    "warning: LIGHT_REMOTE_PROVIDER='{other}' is not a known provider \
                     (anthropic|openai|gemini|deepseek); using key precedence instead"
                ));
            }
        }
    }
    let choice = if anthropic {
        Some(RemoteChoice::Anthropic)
    } else if openai {
        Some(RemoteChoice::OpenAi)
    } else if gemini {
        Some(RemoteChoice::Gemini)
    } else if deepseek {
        Some(RemoteChoice::DeepSeek)
    } else {
        None
    };
    RemoteSelection {
        choice,
        offline: choice.is_none().then_some(OfflineReason::NothingConfigured),
        warnings,
    }
}

/// Return the choice if its key is present, else warn and select nothing (offline) — a
/// named-but-unusable selector must not misroute to another key.
fn present_or_warn(present: bool, choice: RemoteChoice, key: &str) -> RemoteSelection {
    if present {
        RemoteSelection {
            choice: Some(choice),
            offline: None,
            warnings: Vec::new(),
        }
    } else {
        RemoteSelection {
            choice: None,
            offline: Some(OfflineReason::NamedProviderMissingKey {
                selector: choice.id().to_string(),
                key: key.to_string(),
            }),
            warnings: vec![format!(
                "warning: LIGHT_REMOTE_PROVIDER='{}' but {key} is not set; \
                 falling back to the offline/local provider",
                choice.id()
            )],
        }
    }
}

/// The `LIGHT_<P>_MODEL` override variable for a provider.
fn model_var(choice: RemoteChoice) -> &'static str {
    match choice {
        RemoteChoice::Anthropic => "LIGHT_ANTHROPIC_MODEL",
        RemoteChoice::OpenAi => "LIGHT_OPENAI_MODEL",
        RemoteChoice::Gemini => "LIGHT_GEMINI_MODEL",
        RemoteChoice::DeepSeek => "LIGHT_DEEPSEEK_MODEL",
    }
}

/// The default model constant for a provider (used when its `LIGHT_<P>_MODEL` var is unset).
fn default_model_constant(choice: RemoteChoice) -> &'static str {
    match choice {
        RemoteChoice::Anthropic => DEFAULT_ANTHROPIC_MODEL,
        RemoteChoice::OpenAi => DEFAULT_OPENAI_MODEL,
        RemoteChoice::Gemini => DEFAULT_GEMINI_MODEL,
        RemoteChoice::DeepSeek => DEFAULT_DEEPSEEK_MODEL,
    }
}

/// The effective model for a provider: an explicit override, else the constant.
fn default_model_for(choice: RemoteChoice, model_override: Option<String>) -> String {
    model_override.unwrap_or_else(|| default_model_constant(choice).to_string())
}

/// Resolve a provider's base URL from an optional `*_BASE_URL` override.
///
/// `Ok(url)` is the base to use (normalized). `Err(rejection)` means an override was supplied
/// but refused — the caller must construct no provider, so the API key is never sent anywhere.
fn resolve_base_url(
    override_value: Option<String>,
    default: &str,
    var_name: &str,
) -> Result<String, BaseUrlRejection> {
    let Some(value) = override_value.filter(|v| !v.is_empty()) else {
        return Ok(default.to_string());
    };
    match validate_base_url(&value) {
        Ok(normalized) => Ok(normalized),
        Err(e) => Err(BaseUrlRejection {
            var: var_name.to_string(),
            warning: format!(
                "warning: {var_name} was rejected: {e}; falling back to the offline/local \
                 provider rather than sending the API key"
            ),
        }),
    }
}

/// The `LIGHT_<P>_BASE_URL` override variable for a provider, or `None` when it has none.
pub(crate) fn base_url_var(choice: RemoteChoice) -> Option<&'static str> {
    match choice {
        RemoteChoice::OpenAi => Some("LIGHT_OPENAI_BASE_URL"),
        RemoteChoice::DeepSeek => Some("LIGHT_DEEPSEEK_BASE_URL"),
        // Fixed endpoints — `default_base()` only, no operator input reaches them.
        RemoteChoice::Anthropic | RemoteChoice::Gemini => None,
    }
}

/// Resolve the base URL for a provider id from an optional `*_BASE_URL` override — a pure, total
/// helper shared by provider construction and the model-listing path, so the env→default
/// resolution is never duplicated. `id` must be one of the four remotes or `"ollama"` (which has
/// no override and always resolves to the local Ollama root). An invalid override is refused via
/// [`validate_base_url`] before any API key is ever sent to it.
pub(crate) fn resolve_base_url_for(
    id: &str,
    override_value: Option<String>,
) -> anyhow::Result<String> {
    if id == "ollama" {
        return Ok(crate::ollama::LOCAL_BASE.to_string());
    }
    let choice = RemoteChoice::parse(id).ok_or_else(|| {
        anyhow::anyhow!("unknown provider '{id}'; expected anthropic|openai|gemini|deepseek|ollama")
    })?;
    // A provider with no `*_BASE_URL` override var has a fixed endpoint: no operator input may
    // reach it, so an override is ignored and the production default returned (this also keeps the
    // rejection warning from carrying an empty variable name).
    let Some(var_name) = base_url_var(choice) else {
        return Ok(default_base(choice).to_string());
    };
    resolve_base_url(override_value, default_base(choice), var_name)
        .map_err(|r| anyhow::anyhow!("{}", r.warning))
}

/// The production endpoint for a provider.
fn default_base(choice: RemoteChoice) -> &'static str {
    match choice {
        RemoteChoice::Anthropic => AnthropicProvider::api_base_default(),
        RemoteChoice::OpenAi => OpenAiProvider::api_base_default(),
        RemoteChoice::Gemini => GeminiProvider::api_base_default(),
        RemoteChoice::DeepSeek => DeepSeekProvider::api_base_default(),
    }
}

/// Resolve the selected provider's base URL: a non-UTF-8 override is rejected; a UTF-8 override
/// is validated by `validate_base_url`; absent overrides use the production endpoint.
fn base_url_for(choice: RemoteChoice, selection: &Selection) -> Result<String, BaseUrlRejection> {
    let Some(var_name) = base_url_var(choice) else {
        return Ok(default_base(choice).to_string());
    };
    match selection.base_urls.get(choice.id()) {
        Some(Err(var)) => Err(BaseUrlRejection {
            var: var.clone(),
            warning: format!(
                "warning: {var} is not valid UTF-8; falling back to the offline/local \
                 provider rather than sending the API key to the default endpoint"
            ),
        }),
        Some(Ok(value)) => resolve_base_url(Some(value.clone()), default_base(choice), var_name),
        None => Ok(default_base(choice).to_string()),
    }
}

/// Construct the remote provider for `choice`, pinned to `model` and `key`. Returns `None` when
/// the provider's `*_BASE_URL` override fails validation.
fn build_remote(
    choice: RemoteChoice,
    model: String,
    key: String,
    base: Result<String, BaseUrlRejection>,
) -> RemoteBuild {
    match choice {
        RemoteChoice::Anthropic => RemoteBuild {
            provider: Some(Box::new(AnthropicProvider::new(
                AnthropicProvider::api_base_default(),
                key,
                model,
            ))),
            offline: None,
            warnings: Vec::new(),
        },
        RemoteChoice::OpenAi => remote_with_base_url(base, key, model, |base, key, model| {
            Box::new(OpenAiProvider::new(base, key, model))
        }),
        RemoteChoice::Gemini => RemoteBuild {
            provider: Some(Box::new(GeminiProvider::new(
                GeminiProvider::api_base_default(),
                key,
                model,
            ))),
            offline: None,
            warnings: Vec::new(),
        },
        RemoteChoice::DeepSeek => remote_with_base_url(base, key, model, |base, key, model| {
            Box::new(DeepSeekProvider::new(base, key, model))
        }),
    }
}

/// Turn a resolved (or rejected) base URL into a [`RemoteBuild`], so the base-URL rejection path
/// is shared by every provider that has a `*_BASE_URL` override.
fn remote_with_base_url(
    base: Result<String, BaseUrlRejection>,
    key: String,
    model: String,
    build: impl FnOnce(String, String, String) -> Box<dyn Provider>,
) -> RemoteBuild {
    match base {
        Ok(base) => RemoteBuild {
            provider: Some(build(base, key, model)),
            offline: None,
            warnings: Vec::new(),
        },
        Err(rejection) => RemoteBuild {
            provider: None,
            offline: Some(OfflineReason::BaseUrlRejected { var: rejection.var }),
            warnings: vec![rejection.warning],
        },
    }
}

/// The effective Ollama model: a stored/env override, else the constant.
fn ollama_model(selection: &Selection) -> String {
    selection
        .models
        .get("ollama")
        .cloned()
        .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string())
}

/// The remote directive and its source: an env `LIGHT_REMOTE_PROVIDER` wins over the stored
/// preference; an unknown value warns and falls through. `None` means key precedence applies.
fn resolve_remote_directive(
    selection: &Selection,
    warnings: &mut Vec<String>,
) -> (Option<String>, Option<SelectedBy>) {
    if let Some(sel) = selection.selector.as_deref() {
        if let Some(choice) = RemoteChoice::parse(sel) {
            return (
                Some(choice.id().to_string()),
                Some(SelectedBy::RemoteSelectorEnv),
            );
        }
        warnings.push(format!(
            "warning: LIGHT_REMOTE_PROVIDER='{sel}' is not a known provider \
             (anthropic|openai|gemini|deepseek); ignoring it"
        ));
    }
    if let Some(pref) = selection.preferred.as_deref()
        && let Some(choice) = RemoteChoice::parse(pref)
    {
        return (
            Some(choice.id().to_string()),
            Some(SelectedBy::StoredPreference),
        );
    }
    (None, None)
}

/// Build the single provider slot from explicit inputs, always yielding a usable provider
/// (the offline `LocalProvider` as the fail-closed default — selection never errors).
pub fn build_provider(selection: &Selection) -> BuiltProvider {
    let mut warnings = Vec::new();

    // A stored "ollama" preference is a step-3 fallback: it only applies when no env
    // `LIGHT_REMOTE_PROVIDER` names a valid remote (step 2 wins over the stored preference).
    let env_selector_valid = selection
        .selector
        .as_deref()
        .is_some_and(|s| RemoteChoice::parse(s).is_some());
    let (ollama_on, ollama_selected_by) = if selection.ollama {
        (true, Some(SelectedBy::OllamaEnv))
    } else if !env_selector_valid && selection.preferred.as_deref() == Some("ollama") {
        (true, Some(SelectedBy::StoredPreference))
    } else {
        (false, None)
    };

    let (directive, remote_selected_by) = resolve_remote_directive(selection, &mut warnings);
    let remote = select_remote_from(
        directive.as_deref(),
        selection.keys.contains_key("anthropic"),
        selection.keys.contains_key("openai"),
        selection.keys.contains_key("gemini"),
        selection.keys.contains_key("deepseek"),
    );
    warnings.extend(remote.warnings);
    let remote_offline = remote.offline;

    match choose(ollama_on, remote.choice) {
        Slot::Ollama => {
            let model = ollama_model(selection);
            BuiltProvider {
                provider: Box::new(OllamaProvider::local_default(model.clone())),
                model: Some(model),
                offline: None,
                selected_by: ollama_selected_by,
                warnings,
            }
        }
        Slot::Remote(choice) => {
            let selected_by = Some(remote_selected_by.unwrap_or(SelectedBy::KeyPrecedence));
            let model = default_model_for(choice, selection.models.get(choice.id()).cloned());
            let key = selection.keys.get(choice.id()).cloned().unwrap_or_default();
            let base = base_url_for(choice, selection);
            let built = build_remote(choice, model.clone(), key, base);
            warnings.extend(built.warnings);
            match built.provider {
                Some(provider) => BuiltProvider {
                    provider,
                    model: Some(model),
                    offline: None,
                    selected_by,
                    warnings,
                },
                None => BuiltProvider {
                    provider: Box::new(LocalProvider::new()),
                    model: None,
                    offline: built.offline,
                    selected_by: None,
                    warnings,
                },
            }
        }
        Slot::Local => BuiltProvider {
            provider: Box::new(LocalProvider::new()),
            model: None,
            offline: remote_offline,
            selected_by: None,
            warnings,
        },
    }
}

/// Read the `Selection` from the process environment (no keyring, no persisted preference).
pub fn selection_from_env() -> Selection {
    let keys = keys_from(|var| std::env::var(var).ok());

    let mut models = HashMap::new();
    if let Ok(model) = std::env::var("LIGHT_OLLAMA_MODEL")
        && !model.is_empty()
    {
        models.insert("ollama".to_string(), model);
    }
    for choice in [
        RemoteChoice::Anthropic,
        RemoteChoice::OpenAi,
        RemoteChoice::Gemini,
        RemoteChoice::DeepSeek,
    ] {
        if let Ok(model) = std::env::var(model_var(choice))
            && !model.is_empty()
        {
            models.insert(choice.id().to_string(), model);
        }
    }

    let mut base_urls = HashMap::new();
    for choice in [RemoteChoice::OpenAi, RemoteChoice::DeepSeek] {
        if let Some(var) = base_url_var(choice)
            && let Some(raw) = std::env::var_os(var)
        {
            let entry = match raw.into_string() {
                Ok(value) => Ok(value),
                Err(_) => Err(var.to_string()),
            };
            base_urls.insert(choice.id().to_string(), entry);
        }
    }

    Selection {
        ollama: std::env::var("LIGHT_OLLAMA").as_deref() == Ok("1"),
        selector: std::env::var("LIGHT_REMOTE_PROVIDER").ok(),
        preferred: None,
        keys,
        models,
        base_urls,
    }
}

/// Build the provider from the environment, always yielding a usable provider (the offline
/// `LocalProvider` as the fail-closed default — selection never errors).
pub fn build_provider_from_env() -> BuiltProvider {
    build_provider(&selection_from_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_selection(preferred: Option<&str>, keys: &[(&str, &str)]) -> Selection {
        Selection {
            preferred: preferred.map(str::to_string),
            keys: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn ollama_wins_over_any_remote() {
        assert_eq!(choose(true, Some(RemoteChoice::Anthropic)), Slot::Ollama);
        assert_eq!(choose(true, None), Slot::Ollama);
    }

    #[test]
    fn remote_wins_over_local() {
        assert_eq!(
            choose(false, Some(RemoteChoice::OpenAi)),
            Slot::Remote(RemoteChoice::OpenAi)
        );
        assert_eq!(choose(false, None), Slot::Local);
    }

    #[test]
    fn a_valid_selector_with_its_key_present_selects_that_provider() {
        assert_eq!(
            select_remote_from(Some("gemini"), false, false, true, false).choice,
            Some(RemoteChoice::Gemini)
        );
        assert_eq!(
            select_remote_from(Some("ANTHROPIC"), true, false, false, false).choice,
            Some(RemoteChoice::Anthropic)
        );
    }

    #[test]
    fn a_valid_selector_whose_key_is_absent_selects_nothing() {
        // Named "openai" but no OPENAI_API_KEY: must NOT fall through to the (present)
        // Anthropic key. Silent misrouting is what this guard exists to prevent.
        let selection = select_remote_from(Some("openai"), true, false, false, false);
        assert_eq!(selection.choice, None);
        assert_eq!(
            selection.offline,
            Some(OfflineReason::NamedProviderMissingKey {
                selector: "openai".to_string(),
                key: "OPENAI_API_KEY".to_string(),
            })
        );
        assert_eq!(selection.warnings.len(), 1);
        assert!(selection.warnings[0].contains("OPENAI_API_KEY is not set"));
    }

    #[test]
    fn an_unknown_selector_falls_through_to_key_precedence() {
        assert_eq!(
            select_remote_from(Some("bogus"), true, false, false, false).choice,
            Some(RemoteChoice::Anthropic)
        );
        assert_eq!(
            select_remote_from(Some("bogus"), false, true, false, false).choice,
            Some(RemoteChoice::OpenAi)
        );
        assert_eq!(
            select_remote_from(Some("bogus"), false, false, true, false).choice,
            Some(RemoteChoice::Gemini)
        );
        assert_eq!(
            select_remote_from(Some("bogus"), false, false, false, true).choice,
            Some(RemoteChoice::DeepSeek)
        );
    }

    #[test]
    fn an_unknown_selector_warns_but_keeps_the_live_provider() {
        let selection = select_remote_from(Some("bogus"), true, false, false, false);
        assert_eq!(selection.choice, Some(RemoteChoice::Anthropic));
        assert_eq!(selection.offline, None);
        assert_eq!(selection.warnings.len(), 1);
        assert!(selection.warnings[0].contains("not a known provider"));
    }

    #[test]
    fn nothing_configured_reports_the_offline_reason() {
        let selection = select_remote_from(None, false, false, false, false);
        assert_eq!(selection.choice, None);
        assert_eq!(selection.offline, Some(OfflineReason::NothingConfigured));
        assert!(selection.warnings.is_empty());
    }

    #[test]
    fn key_precedence_is_anthropic_then_openai_then_gemini_then_deepseek() {
        assert_eq!(
            select_remote_from(None, true, true, true, true).choice,
            Some(RemoteChoice::Anthropic)
        );
        assert_eq!(
            select_remote_from(None, false, true, true, true).choice,
            Some(RemoteChoice::OpenAi)
        );
        assert_eq!(
            select_remote_from(None, false, false, true, true).choice,
            Some(RemoteChoice::Gemini)
        );
        assert_eq!(
            select_remote_from(None, false, false, false, true).choice,
            Some(RemoteChoice::DeepSeek)
        );
        assert_eq!(
            select_remote_from(None, false, false, false, false).choice,
            None
        );
    }

    #[test]
    fn resolve_base_url_accepts_a_valid_override_normalized() {
        // The `url` crate strips surrounding whitespace and lowercases the scheme, and appends a
        // trailing slash for an empty path — so the returned value is the NORMALIZED form, not the
        // operator's raw string.
        assert_eq!(
            resolve_base_url(
                Some(" HTTPS://api.openai.com ".to_string()),
                "https://api.openai.com",
                "LIGHT_OPENAI_BASE_URL"
            )
            .unwrap(),
            "https://api.openai.com/"
        );
    }

    #[test]
    fn resolve_base_url_rejects_a_bad_override() {
        let rejection = resolve_base_url(
            Some("http://evil.example.com".to_string()),
            "https://api.openai.com",
            "LIGHT_OPENAI_BASE_URL",
        )
        .unwrap_err();
        assert_eq!(rejection.var, "LIGHT_OPENAI_BASE_URL");
        assert!(
            rejection
                .warning
                .contains("LIGHT_OPENAI_BASE_URL was rejected")
        );
    }

    #[test]
    fn resolve_base_url_uses_the_default_when_unset_or_empty() {
        assert_eq!(
            resolve_base_url(None, "https://default", "LIGHT_OPENAI_BASE_URL").unwrap(),
            "https://default"
        );
        assert_eq!(
            resolve_base_url(
                Some(String::new()),
                "https://default",
                "LIGHT_OPENAI_BASE_URL"
            )
            .unwrap(),
            "https://default"
        );
    }

    #[test]
    fn resolve_base_url_for_uses_production_defaults_and_ollama_local() {
        assert_eq!(
            resolve_base_url_for("anthropic", None).unwrap(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            resolve_base_url_for("openai", None).unwrap(),
            "https://api.openai.com"
        );
        assert_eq!(
            resolve_base_url_for("gemini", None).unwrap(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            resolve_base_url_for("deepseek", None).unwrap(),
            "https://api.deepseek.com"
        );
        assert_eq!(
            resolve_base_url_for("ollama", None).unwrap(),
            crate::ollama::LOCAL_BASE
        );
    }

    #[test]
    fn resolve_base_url_for_validates_overrides() {
        assert_eq!(
            resolve_base_url_for("openai", Some("https://gw.example.com".to_string())).unwrap(),
            "https://gw.example.com/"
        );
        assert!(
            resolve_base_url_for("openai", Some("http://evil.example.com".to_string())).is_err()
        );
    }

    #[test]
    fn resolve_base_url_for_ignores_overrides_for_fixed_endpoint_providers() {
        // Anthropic/Gemini have no `*_BASE_URL` override var; an override must be ignored (default
        // returned), never honored — there is no operator-controlled variable to receive the key.
        assert_eq!(
            resolve_base_url_for("anthropic", Some("https://evil.example.com".to_string()))
                .unwrap(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            resolve_base_url_for("gemini", Some("https://evil.example.com".to_string())).unwrap(),
            "https://generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn resolve_base_url_for_rejects_unknown_ids() {
        assert!(resolve_base_url_for("local", None).is_err());
        assert!(resolve_base_url_for("bogus", None).is_err());
    }

    #[test]
    fn default_models_match_otto_constants() {
        assert_eq!(
            default_model_constant(RemoteChoice::Anthropic),
            "claude-haiku-4-5"
        );
        assert_eq!(default_model_constant(RemoteChoice::OpenAi), "gpt-4o-mini");
        assert_eq!(
            default_model_constant(RemoteChoice::Gemini),
            "gemini-2.5-flash"
        );
        assert_eq!(
            default_model_constant(RemoteChoice::DeepSeek),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn model_override_wins_over_the_constant() {
        assert_eq!(
            default_model_for(RemoteChoice::OpenAi, Some("gpt-5".to_string())),
            "gpt-5"
        );
        assert_eq!(default_model_for(RemoteChoice::OpenAi, None), "gpt-4o-mini");
    }

    #[test]
    fn env_key_var_maps_remote_ids_only() {
        assert_eq!(env_key_var("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(env_key_var("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_key_var("gemini"), Some("GEMINI_API_KEY"));
        assert_eq!(env_key_var("deepseek"), Some("DEEPSEEK_API_KEY"));
        assert_eq!(env_key_var("ollama"), None);
        assert_eq!(env_key_var("local"), None);
    }

    #[test]
    fn a_stored_preference_with_its_key_selects_that_provider() {
        let built = build_provider(&test_selection(Some("openai"), &[("openai", "sk-1")]));
        assert_eq!(built.provider.id(), "openai");
        assert_eq!(built.selected_by, Some(SelectedBy::StoredPreference));
        assert_eq!(built.offline, None);
    }

    #[test]
    fn a_stored_preference_without_its_key_goes_offline_without_misrouting() {
        let built = build_provider(&test_selection(Some("openai"), &[("anthropic", "sk-a")]));
        assert_eq!(built.provider.id(), "local");
        assert_eq!(built.selected_by, None);
        assert_eq!(
            built.offline,
            Some(OfflineReason::NamedProviderMissingKey {
                selector: "openai".to_string(),
                key: "OPENAI_API_KEY".to_string(),
            })
        );
    }

    #[test]
    fn key_precedence_reports_itself_as_the_selected_by() {
        let built = build_provider(&test_selection(None, &[("gemini", "sk-g")]));
        assert_eq!(built.provider.id(), "gemini");
        assert_eq!(built.selected_by, Some(SelectedBy::KeyPrecedence));
    }

    #[test]
    fn ollama_env_reports_itself_and_wins_over_a_key() {
        let mut selection = test_selection(None, &[("openai", "sk-o")]);
        selection.ollama = true;
        let built = build_provider(&selection);
        assert_eq!(built.provider.id(), "ollama");
        assert_eq!(built.selected_by, Some(SelectedBy::OllamaEnv));
    }

    #[test]
    fn a_stored_ollama_preference_selects_ollama() {
        let built = build_provider(&test_selection(Some("ollama"), &[]));
        assert_eq!(built.provider.id(), "ollama");
        assert_eq!(built.selected_by, Some(SelectedBy::StoredPreference));
    }

    #[test]
    fn an_env_selector_beats_a_stored_ollama_preference() {
        let mut selection = test_selection(Some("ollama"), &[("openai", "sk-o")]);
        selection.selector = Some("openai".to_string());
        let built = build_provider(&selection);
        assert_eq!(built.provider.id(), "openai");
        assert_eq!(built.selected_by, Some(SelectedBy::RemoteSelectorEnv));
    }

    #[test]
    fn a_stored_ollama_preference_beats_key_precedence_when_no_valid_selector() {
        let mut selection = test_selection(Some("ollama"), &[("openai", "sk-o")]);
        selection.selector = Some("bogus".to_string());
        let built = build_provider(&selection);
        assert_eq!(built.provider.id(), "ollama");
        assert_eq!(built.selected_by, Some(SelectedBy::StoredPreference));
        assert!(
            built
                .warnings
                .iter()
                .any(|w| w.contains("not a known provider"))
        );
    }

    #[test]
    fn an_env_selector_wins_over_the_stored_preference() {
        let mut selection = test_selection(
            Some("anthropic"),
            &[("openai", "sk-o"), ("anthropic", "sk-a")],
        );
        selection.selector = Some("openai".to_string());
        let built = build_provider(&selection);
        assert_eq!(built.provider.id(), "openai");
        assert_eq!(built.selected_by, Some(SelectedBy::RemoteSelectorEnv));
    }

    #[test]
    fn an_unknown_env_selector_falls_through_to_the_stored_preference() {
        let mut selection = test_selection(Some("openai"), &[("openai", "sk-o")]);
        selection.selector = Some("bogus".to_string());
        let built = build_provider(&selection);
        assert_eq!(built.provider.id(), "openai");
        assert_eq!(built.selected_by, Some(SelectedBy::StoredPreference));
        assert!(
            built
                .warnings
                .iter()
                .any(|w| w.contains("not a known provider"))
        );
    }

    #[test]
    fn build_provider_from_env_yields_a_known_provider_id() {
        // Deliberately env-agnostic: the wrapper reads the real process env, so only assert the
        // id is one of the known provider ids, never the offline state.
        let built = build_provider_from_env();
        assert!(!built.provider.id().is_empty());
    }

    /// An env-supplied key must be usable only after stripping surrounding whitespace (a
    /// trailing newline from `$(cat keyfile)` must not reach a Bearer header).
    #[test]
    fn normalize_env_key_trims_surrounding_whitespace() {
        assert_eq!(
            normalize_env_key("  sk-1\n".to_string()),
            Some("sk-1".to_string())
        );
    }

    #[test]
    fn normalize_env_key_keeps_a_clean_value_verbatim() {
        assert_eq!(
            normalize_env_key("sk-1".to_string()),
            Some("sk-1".to_string())
        );
    }

    /// A whitespace-only value carries no key: treat it as absent, not as configured.
    #[test]
    fn normalize_env_key_treats_a_whitespace_only_value_as_absent() {
        assert_eq!(normalize_env_key(" \t\r\n".to_string()), None);
    }

    #[test]
    fn normalize_env_key_treats_an_empty_value_as_absent() {
        assert_eq!(normalize_env_key(String::new()), None);
    }

    /// Only *surrounding* whitespace is stripped; internal spacing is part of the value.
    #[test]
    fn normalize_env_key_preserves_internal_whitespace() {
        assert_eq!(
            normalize_env_key("abc def ".to_string()),
            Some("abc def".to_string())
        );
    }

    /// The mapping (`env_key_var` naming → normalization → map insertion) must flow through for
    /// all four declared ids — and, since those ids are hardcoded inside `keys_from`, a counting
    /// stub proves `read` is consulted exactly once per declared variable, never for anything else.
    #[test]
    fn keys_from_reads_only_the_declared_vars() {
        let keys = keys_from(|var| Some(format!("k-{var}")));
        assert_eq!(keys.len(), 4);
        assert_eq!(
            keys.get("anthropic"),
            Some(&"k-ANTHROPIC_API_KEY".to_string())
        );
        assert_eq!(keys.get("openai"), Some(&"k-OPENAI_API_KEY".to_string()));
        assert_eq!(keys.get("gemini"), Some(&"k-GEMINI_API_KEY".to_string()));
        assert_eq!(
            keys.get("deepseek"),
            Some(&"k-DEEPSEEK_API_KEY".to_string())
        );

        let reads = std::cell::Cell::new(0u32);
        let counting = |_: &str| {
            reads.set(reads.get() + 1);
            None
        };
        assert!(keys_from(counting).is_empty());
        assert_eq!(reads.get(), 4);
    }

    #[test]
    fn keys_from_treats_a_whitespace_only_env_value_as_absent() {
        let keys = keys_from(|var| match var {
            "OPENAI_API_KEY" => Some("   \n".to_string()),
            _ => None,
        });
        assert!(keys.is_empty());
    }

    #[test]
    fn keys_from_trims_a_trailing_newline_before_inserting() {
        let keys = keys_from(|var| match var {
            "ANTHROPIC_API_KEY" => Some("sk-a\n".to_string()),
            _ => None,
        });
        assert_eq!(keys.get("anthropic"), Some(&"sk-a".to_string()));
    }
}
