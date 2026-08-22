//! Display-only provider metadata: the active provider's id/model plus why it was selected and
//! any offline reason. Construction (composition) lives in `crate::selection`, not here.

use light_factory_providers::{OfflineReason, SelectedBy};

use crate::i18n::{self, Locale};

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

/// The active provider's id, model, selection reason, and offline status, for the connected
/// header and the `/provider` listing.
#[derive(Clone)]
pub struct ProviderInfo {
    pub id: String,
    pub model: Option<String>,
    /// `Some(reason)` when the offline `LocalProvider` was selected; `None` for a live provider.
    pub offline: Option<OfflineReason>,
    /// Which rule selected a live provider; `None` when offline.
    pub selected_by: Option<SelectedBy>,
    /// Human-readable selection warnings, for the engine pane to surface.
    pub warnings: Vec<String>,
    /// Providers whose stored key could not be read. Empty when the store answered for all of
    /// them — including when it answered "no key".
    pub store_failures: Vec<StoreFailure>,
}

impl ProviderInfo {
    /// Render as `id` or `id (model)`.
    pub fn display(&self) -> String {
        match &self.model {
            Some(model) => format!("{} ({model})", self.id),
            None => self.id.clone(),
        }
    }

    /// A short localized phrase explaining why this provider is active (e.g. "key precedence",
    /// "stored preference", "offline"), or an empty string when there is nothing to add.
    pub fn reason(&self, locale: Locale) -> String {
        if self.offline.is_some() {
            return i18n::t(locale, "provider.reason.offline").to_string();
        }
        match self.selected_by {
            Some(SelectedBy::OllamaEnv) => {
                i18n::t(locale, "provider.reason.ollama_env").to_string()
            }
            Some(SelectedBy::RemoteSelectorEnv) => {
                i18n::t(locale, "provider.reason.selector_env").to_string()
            }
            Some(SelectedBy::StoredPreference) => {
                i18n::t(locale, "provider.reason.stored").to_string()
            }
            Some(SelectedBy::KeyPrecedence) => {
                i18n::t(locale, "provider.reason.key_precedence").to_string()
            }
            None => String::new(),
        }
    }

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

/// Map an [`OfflineReason`] to a localized notice naming the variable(s) to set.
pub fn offline_notice(locale: Locale, reason: &OfflineReason) -> String {
    match reason {
        OfflineReason::NothingConfigured => i18n::t(locale, "provider.offline.nothing").to_string(),
        OfflineReason::NamedProviderMissingKey { selector, key } => i18n::t_with(
            locale,
            "provider.offline.missing_key",
            &[("selector", selector), ("key", key)],
        ),
        OfflineReason::BaseUrlRejected { var } => {
            i18n::t_with(locale, "provider.offline.base_url", &[("var", var)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(offline: Option<OfflineReason>, selected_by: Option<SelectedBy>) -> ProviderInfo {
        ProviderInfo {
            id: "openai".to_string(),
            model: Some("gpt-4o-mini".to_string()),
            offline,
            selected_by,
            warnings: Vec::new(),
            store_failures: Vec::new(),
        }
    }

    fn failure(provider: &str) -> StoreFailure {
        StoreFailure {
            provider: provider.to_string(),
            error: "locked".to_string(),
        }
    }

    #[test]
    fn offline_notice_covers_each_reason() {
        assert_eq!(
            offline_notice(Locale::En, &OfflineReason::NothingConfigured),
            "No provider configured — set ANTHROPIC_API_KEY (or another provider's key) or LIGHT_OLLAMA=1"
        );
        assert_eq!(
            offline_notice(
                Locale::En,
                &OfflineReason::NamedProviderMissingKey {
                    selector: "openai".into(),
                    key: "OPENAI_API_KEY".into(),
                }
            ),
            "Provider 'openai' selected but OPENAI_API_KEY is not set — falling back to offline"
        );
        assert_eq!(
            offline_notice(
                Locale::En,
                &OfflineReason::BaseUrlRejected {
                    var: "LIGHT_OPENAI_BASE_URL".into(),
                }
            ),
            "LIGHT_OPENAI_BASE_URL was rejected — falling back to offline"
        );
    }

    #[test]
    fn display_appends_the_model_when_present() {
        assert_eq!(info(None, None).display(), "openai (gpt-4o-mini)");
    }

    #[test]
    fn reason_names_the_selection_source() {
        assert_eq!(
            info(None, Some(SelectedBy::KeyPrecedence)).reason(Locale::En),
            "key precedence"
        );
        assert_eq!(
            info(None, Some(SelectedBy::StoredPreference)).reason(Locale::En),
            "stored preference"
        );
        assert_eq!(
            info(None, Some(SelectedBy::OllamaEnv)).reason(Locale::En),
            "LIGHT_OLLAMA"
        );
        assert_eq!(
            info(None, Some(SelectedBy::RemoteSelectorEnv)).reason(Locale::En),
            "LIGHT_REMOTE_PROVIDER"
        );
    }

    #[test]
    fn reason_reports_offline() {
        assert_eq!(
            info(Some(OfflineReason::NothingConfigured), None).reason(Locale::En),
            "offline"
        );
    }

    #[test]
    fn reason_is_empty_without_a_source() {
        assert_eq!(info(None, None).reason(Locale::En), "");
    }

    /// The issue's third acceptance criterion: an offline fallback caused by an unreadable store
    /// must say so, instead of telling the user to set a key they already set.
    #[test]
    fn a_store_failure_replaces_the_nothing_configured_notice() {
        let mut info = info(Some(OfflineReason::NothingConfigured), None);
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        assert!(
            notices
                .iter()
                .any(|n| n.contains("openai") && n.contains("locked")),
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
            notices
                .iter()
                .any(|n| n.contains("openai") && n.contains("locked")),
            "the store failure is still reported on its own line: {notices:?}"
        );
    }

    #[test]
    fn notices_without_a_store_failure_are_unchanged() {
        let info = info(Some(OfflineReason::NothingConfigured), None);
        assert_eq!(
            info.notices(Locale::En),
            vec![offline_notice(
                Locale::En,
                &OfflineReason::NothingConfigured
            )]
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
}
