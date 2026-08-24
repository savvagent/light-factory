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
    /// The offline line is substituted exactly when [`ProviderInfo::store_caused_offline`] says
    /// the store is why there is no key, and it becomes *two* lines — cause and remedy — because
    /// the surface truncates.
    ///
    /// [`OfflineReason::NamedProviderMissingKey`] is the imprecise corner: `LIGHT_REMOTE_PROVIDER`
    /// naming a provider whose stored key could not be read still reports "{key} is not set",
    /// which is true of the variable but silent about the store. The line above it names the real
    /// cause, so the user is not misdirected; sharpening the wording is savvagent/light-factory#67.
    pub fn notices(&self, locale: Locale) -> Vec<String> {
        let mut lines = self.warnings.clone();
        // A locked wallet or a dead session bus fails every entry with the same words, so the
        // per-provider form would be four rows saying one thing. Keep it only when the causes
        // actually differ, which is the partial failure `KeyringStore`'s per-entry reads allow.
        //
        // The lone failure is its own arm on purpose. `split_first` leaves an empty `rest`, and
        // `rest.iter().all(..)` is vacuously true on it, so one failure used to take the collapsed
        // branch and lose the provider name — in exactly the case where naming it matters most.
        match self.store_failures.as_slice() {
            [] => {}
            [only] => lines.push(i18n::t_with(
                locale,
                "provider.store.unavailable",
                &[("provider", &only.provider), ("error", &only.error)],
            )),
            [first, rest @ ..] if rest.iter().all(|f| f.error == first.error) => {
                lines.push(i18n::t_with(
                    locale,
                    "provider.store.unavailable_all",
                    &[("error", &first.error)],
                ));
            }
            all => {
                for failure in all {
                    lines.push(i18n::t_with(
                        locale,
                        "provider.store.unavailable",
                        &[("provider", &failure.provider), ("error", &failure.error)],
                    ));
                }
            }
        }
        if let Some(reason) = &self.offline {
            if self.store_caused_offline().is_empty() {
                lines.push(offline_notice(locale, reason));
            } else {
                // Two lines, not one sentence: `draw_engine` renders each line as a `ListItem` in
                // a ratatui `List`, which truncates rather than wraps. A remedy appended to the
                // cause is a remedy the user never sees.
                lines.push(i18n::t(locale, "provider.offline.store_unavailable").to_string());
                lines.push(i18n::t(locale, "provider.offline.store_remedy").to_string());
            }
        }
        lines
    }

    /// The store failures, but only when the store is why there is no key.
    ///
    /// `notices` and `/models` both have to answer this, and answering it twice is how they
    /// drifted apart: one branched exhaustively on the reason, the other ignored it entirely and
    /// told a user whose `*_BASE_URL` was rejected to unlock a keyring that was never in the way.
    ///
    /// [`OfflineReason::NothingConfigured`] is the one reason the store can *cause*: with an
    /// unreadable store you cannot know whether a key existed, so "no provider configured" states
    /// as fact the very thing the failure left unknown. The other reasons carry their own cause,
    /// and overwriting one would repeat that defect in the other direction — the failures are
    /// still reported on their own lines above either way.
    pub fn store_caused_offline(&self) -> &[StoreFailure] {
        match self.offline {
            Some(OfflineReason::NothingConfigured) => &self.store_failures,
            Some(
                OfflineReason::NamedProviderMissingKey { .. }
                | OfflineReason::BaseUrlRejected { .. },
            )
            | None => &[],
        }
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
    use crate::selection::REMOTE_IDS;

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
            "No provider key — set ANTHROPIC_API_KEY or LIGHT_OLLAMA=1"
        );
        assert_eq!(
            offline_notice(
                Locale::En,
                &OfflineReason::NamedProviderMissingKey {
                    selector: "openai".into(),
                    key: "OPENAI_API_KEY".into(),
                }
            ),
            "Provider 'openai': OPENAI_API_KEY is not set — offline"
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
    ///
    /// A lone failure names the provider: `KeyringStore` reads per entry, so one item failing is
    /// a real state, and it is the case where naming it matters most.
    #[test]
    fn a_store_failure_replaces_the_nothing_configured_notice() {
        let mut info = info(Some(OfflineReason::NothingConfigured), None);
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        assert!(
            notices
                .iter()
                .any(|n| n.contains("openai") && n.contains("locked")),
            "the provider and the cause must both be named: {notices:?}"
        );
        assert!(
            !notices.iter().any(|n| n.contains("No provider key")),
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
            notices.iter().any(|n| n.contains("locked")),
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

    /// The substituted line replaced one that named a remedy. A broken keyring is exactly when an
    /// environment variable helps, so the replacement must not be a dead end — and the remedy gets
    /// its own line, so `draw_engine`'s truncating `List` gives it a row of its own. That it
    /// survives the render is asserted by `the_store_offline_remedy_survives_an_80_column_render`.
    #[test]
    fn the_store_offline_notice_still_names_a_remedy() {
        let mut info = info(Some(OfflineReason::NothingConfigured), None);
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        let remedy = notices
            .last()
            .expect("an offline provider has an offline line");
        assert!(
            remedy.contains("ANTHROPIC_API_KEY"),
            "no remedy named: {notices:?}"
        );
        assert!(
            notices.iter().any(|n| n.contains("credential store")),
            "the cause line is missing: {notices:?}"
        );
    }

    /// A dead D-Bus session fails every entry with the same words. One line per provider is four
    /// rows saying one thing; collapse them while keeping the per-provider form when the causes
    /// genuinely differ.
    #[test]
    fn identical_store_failures_collapse_to_one_line() {
        let mut info = info(Some(OfflineReason::NothingConfigured), None);
        info.store_failures = REMOTE_IDS
            .iter()
            .map(|id| StoreFailure {
                provider: id.to_string(),
                error: "no D-Bus session".into(),
            })
            .collect();
        let notices = info.notices(Locale::En);
        assert_eq!(
            notices.len(),
            3,
            "one collapsed failure line plus the offline cause and remedy: {notices:?}"
        );
        assert!(notices[0].contains("no D-Bus session"), "{notices:?}");
        assert!(
            !notices[0].contains("anthropic"),
            "the collapsed line names no provider: {notices:?}"
        );
    }

    #[test]
    fn differing_store_failures_are_reported_per_provider() {
        let mut info = info(None, Some(SelectedBy::KeyPrecedence));
        info.store_failures = vec![
            StoreFailure {
                provider: "openai".into(),
                error: "locked".into(),
            },
            StoreFailure {
                provider: "gemini".into(),
                error: "no D-Bus session".into(),
            },
        ];
        let notices = info.notices(Locale::En);
        assert_eq!(notices.len(), 2);
        assert!(
            notices
                .iter()
                .any(|n| n.contains("openai") && n.contains("locked"))
        );
        assert!(
            notices
                .iter()
                .any(|n| n.contains("gemini") && n.contains("no D-Bus session"))
        );
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
