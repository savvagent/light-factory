//! Compose the active provider from the environment, persisted preferences, and the OS keyring,
//! and expose the pieces the commands need (`key_status`, `rebuild`).

use std::sync::Arc;

use light_factory_providers::{
    Provider, Selection, build_provider, env_key_var, selection_from_env,
};
use light_factory_tui::credentials::CredentialStore;

use crate::provider::{ProviderInfo, StoreFailure};
use crate::settings::Settings;
use crate::text::one_line;

/// The remote provider ids, in key-precedence order.
pub const REMOTE_IDS: [&str; 4] = ["anthropic", "openai", "gemini", "deepseek"];

/// Whether a provider authenticates with an API key at all. `ollama` and the offline `local`
/// provider do not, so `/key` refuses them and the connect modal skips straight to the model list.
pub fn takes_key(provider: &str) -> bool {
    env_key_var(provider).is_some()
}

/// Read a named environment variable. The only place this module touches the real environment:
/// the public [`key_status`] and [`resolve_key`] pass it to their `_with` forms, and tests pass a
/// stub instead.
fn process_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// The env-supplied key for `provider`, if the environment supplies a usable one.
///
/// An empty value is treated as absent, so the connect flow never fetches with an empty key. Both
/// [`key_status_with`] and [`resolve_key_with`] go through here, so the rule has one source of
/// truth rather than being restated at each of them.
fn env_key(provider: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env_key_var(provider)
        .and_then(env)
        .filter(|k| !k.is_empty())
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
            KeyResolution::Unavailable(error) => f.debug_tuple("Unavailable").field(error).finish(),
        }
    }
}

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

/// Build the provider and its display record from an explicit [`Selection`]. Pure.
fn build_and_info(selection: &Selection) -> (Arc<dyn Provider>, ProviderInfo) {
    let built = build_provider(selection);
    let id = built.provider.id().to_string();
    let info = ProviderInfo {
        id,
        model: built.model,
        offline: built.offline,
        selected_by: built.selected_by,
        warnings: built.warnings,
        store_failures: Vec::new(),
    };
    (Arc::from(built.provider), info)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use light_factory_providers::SelectedBy;
    use light_factory_tui::credentials::FailingStore;
    use light_factory_tui::credentials::MemStore;

    fn settings(provider: Option<&str>) -> Settings {
        Settings {
            lang: "en".to_string(),
            provider: provider.map(str::to_string),
            models: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn non_remote_providers_have_no_key() {
        let store = MemStore::new();
        assert_eq!(key_status("ollama", &store), KeyStatus::None);
        assert_eq!(key_status("local", &store), KeyStatus::None);
    }

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

    #[test]
    fn apply_preferences_maps_preferences_and_keyring_keys() {
        let store = MemStore::new();
        store.set("openai", "sk-o").unwrap();
        let base = Selection::default();
        let (selection, _failures) = apply_preferences(base, &settings(Some("openai")), &store);
        assert_eq!(selection.preferred.as_deref(), Some("openai"));
        assert_eq!(selection.keys.get("openai"), Some(&"sk-o".to_string()));
    }

    #[test]
    fn apply_preferences_does_not_overwrite_an_env_key() {
        let store = MemStore::new();
        store.set("openai", "sk-ring").unwrap();
        let mut base = Selection::default();
        base.keys.insert("openai".to_string(), "sk-env".to_string());
        let (selection, _failures) = apply_preferences(base, &settings(None), &store);
        assert_eq!(selection.keys.get("openai"), Some(&"sk-env".to_string()));
    }

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

    #[test]
    fn build_and_info_with_nothing_configured_is_offline_local() {
        let (provider, info) = build_and_info(&Selection::default());
        assert_eq!(provider.id(), "local");
        assert_eq!(
            info.offline,
            Some(light_factory_providers::OfflineReason::NothingConfigured)
        );
        assert_eq!(info.selected_by, None);
    }

    #[test]
    fn build_and_info_with_a_stored_key_selects_that_provider() {
        let mut base = Selection {
            preferred: Some("deepseek".to_string()),
            ..Default::default()
        };
        base.keys.insert("deepseek".to_string(), "sk-d".to_string());
        let (provider, info) = build_and_info(&base);
        assert_eq!(provider.id(), "deepseek");
        assert_eq!(info.selected_by, Some(SelectedBy::StoredPreference));
    }

    #[test]
    fn build_and_info_uses_key_precedence_without_a_preference() {
        let mut base = Selection::default();
        base.keys
            .insert("anthropic".to_string(), "sk-a".to_string());
        let (provider, info) = build_and_info(&base);
        assert_eq!(provider.id(), "anthropic");
        assert_eq!(info.selected_by, Some(SelectedBy::KeyPrecedence));
    }
}
