//! The modal overlays layered over the TUI's screens: `/connect`, `/models`, and help.
//!
//! Each modal is a state enum plus a pure key-transition function, so the whole state machine is
//! testable without a terminal, a keyring, or the network. `App` owns at most one of them at a
//! time — the normal state is none open, which is what `Option<Modal>` says.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures_util::FutureExt;
use light_factory_providers::{OfflineReason, list_models, list_ollama_models};
use light_factory_tui::credentials::CredentialStore;
use light_factory_tui::i18n::{self, Locale};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

// `takes_key` also gates the `/key` command; `mask` (below) also renders the `Mode::Key` screen.
// Both point away from `app.rs`, so this module has no edge back into the one that owns it.
use crate::selection::takes_key;

/// Mask a secret for rendering: one `*` per character, never the input value.
pub(crate) fn mask(input: &str) -> String {
    "*".repeat(input.chars().count())
}

/// Whether a provider row has a key behind it.
///
/// Three states rather than a boolean because the failure that motivated them is a store that
/// cannot answer: rendering that as "no key" tells the user to store a key they may already have
/// stored, and the false branch is the one that routes to key entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKey {
    /// A key is available, from the environment or the store.
    Present,
    /// The store answered, and there is no key.
    Absent,
    /// The store could not be read, so whether a key exists is unknown.
    Unavailable,
}

/// One row of the connect modal's provider list. Self-contained (id + key state) so the pure
/// transition can decide navigation without touching the keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRow {
    pub(crate) id: String,
    pub(crate) key: RowKey,
}

/// The connect modal's step. `rows` is carried through every step so "back" navigation can
/// reconstruct the provider list without re-querying the keyring — which is what keeps
/// [`connect_step_next`] pure.
///
/// The cost of that purity: `rows` is a snapshot taken when the modal opened. After a key is
/// written on [`ConnectStep::KeyEntry`], stepping back to [`ConnectStep::ProviderList`] still
/// renders that provider as unconnected. Re-reading the keyring to correct the display would put
/// I/O back into the transition; fix it, if it is ever worth fixing, by restating `rows` at the
/// call site that owns the store, not from inside here.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ConnectStep {
    ProviderList {
        rows: Vec<ProviderRow>,
        selected: usize,
    },
    KeyEntry {
        rows: Vec<ProviderRow>,
        provider: String,
        input: String,
    },
    ModelList {
        rows: Vec<ProviderRow>,
        provider: String,
        models: Vec<String>,
        selected: usize,
        fetching: bool,
        error: Option<String>,
        /// Whether this list was reached by typing a key rather than from an already-connected
        /// provider. It decides where Esc goes: back to [`ConnectStep::KeyEntry`] when
        /// `takes_key(provider) && (from_key || error.is_some())`, so a user who has just mistyped
        /// a key lands on the field to retype it instead of on the provider list.
        from_key: bool,
    },
}

impl std::fmt::Debug for ConnectStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `KeyEntry.input` holds a plaintext API key and must never appear in a Debug rendering.
        match self {
            ConnectStep::ProviderList { rows, selected } => f
                .debug_struct("ProviderList")
                .field("rows", rows)
                .field("selected", selected)
                .finish(),
            ConnectStep::KeyEntry {
                rows,
                provider,
                input: _,
            } => f
                .debug_struct("KeyEntry")
                .field("rows", rows)
                .field("provider", provider)
                .field("input", &"<redacted>")
                .finish(),
            ConnectStep::ModelList {
                rows,
                provider,
                models,
                selected,
                fetching,
                error,
                from_key,
            } => f
                .debug_struct("ModelList")
                .field("rows", rows)
                .field("provider", provider)
                .field("models", models)
                .field("selected", selected)
                .field("fetching", fetching)
                .field("error", error)
                .field("from_key", from_key)
                .finish(),
        }
    }
}

/// The `/models` modal's step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ModelsStep {
    ModelList {
        provider: String,
        models: Vec<String>,
        selected: usize,
        fetching: bool,
    },
    /// A transport-class fetch failure: the list is unavailable but a typed id may still be right,
    /// so the modal offers a retry plus an explicitly-unverified manual entry.
    Manual {
        provider: String,
        input: String,
        error: Option<String>,
    },
    /// A credential-class fetch failure (no key resolved, the provider refused the one we sent,
    /// or the credential store could not be read). Typing a model id cannot repair a credential,
    /// so this step shows a remedy and takes no input.
    ///
    /// The step carries the failure *class*, not a pre-localized remedy sentence: the remedy is
    /// looked up at render time so `/lang` cannot leave it stale on an open modal, and the
    /// class-based branch stays in the module that defines [`FetchFailure`].
    Credentials {
        provider: String,
        error: String,
        class: FetchFailure,
    },
    Offline,
}

/// Why a model-list fetch failed, in the only terms the modal has to act on.
///
/// `pub(crate)` is required, not incidental: `UiEvent` is `pub` and carries these in
/// `ModelsFetched`, so a fully-private field type trips the `private_interfaces` lint. Do not
/// "tidy" it to private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchFailure {
    /// No API key could be resolved for the provider at all.
    MissingKey,
    /// The provider refused the credential we sent (401/403).
    Auth,
    /// Anything else: DNS, refused connection, TLS, timeout, 5xx, malformed body.
    Fetch,
    /// The credential store could not be read, so whether a key exists is unknown. Distinct from
    /// [`FetchFailure::MissingKey`]: the remedy for a missing key is to store one, which is not a
    /// remedy for a store that cannot be read.
    StoreUnavailable,
}

impl FetchFailure {
    /// The i18n key of the remedy for this class, or `None` when the remedy is simply to retry.
    ///
    /// This is the single statement of "what can the user do about it": `needs_credentials` is
    /// `remedy_key().is_some()`, and both render sites look the string up here rather than
    /// carrying a pre-localized copy that `/lang` would leave stale.
    pub(crate) fn remedy_key(self) -> Option<&'static str> {
        match self {
            // `/connect` and `/key` both write to the credential store, so neither is a remedy
            // for a store that cannot be read.
            FetchFailure::StoreUnavailable => Some("models.store_remedy"),
            FetchFailure::MissingKey | FetchFailure::Auth => Some("models.credentials_remedy"),
            FetchFailure::Fetch => None,
        }
    }

    /// Whether the remedy is a credential (`/connect`, `/key`) rather than a retry. The single
    /// predicate the modal branches on, so a future class only has to answer this question —
    /// which it must, in [`FetchFailure::remedy_key`]'s exhaustive match.
    pub(crate) fn needs_credentials(self) -> bool {
        self.remedy_key().is_some()
    }
}

/// A failed model-list fetch: the class the modal branches on, plus the detail to render. The
/// detail is always produced by [`summarize_provider_error`], so it is one bounded line.
///
/// `pub(crate)` for the same reason as [`FetchFailure`]: it appears in the `pub` `UiEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchError {
    pub(crate) class: FetchFailure,
    pub(crate) message: String,
}

/// The one overlay that owns the keyboard, if any.
///
/// One variant per modal is what makes "two modals open at once" unrepresentable: `App` holds a
/// single [`ModalHost`], not one `Option` per modal.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Modal {
    Help,
    Connect(ConnectStep),
    Models(ModelsStep),
}

/// What a [`ModalTransition::Apply`] commits. The two modals apply different things, at different
/// privilege: `/connect` adopts a new preferred provider — changing which service receives the
/// user's prompts and key — while `/models` only re-pins the model of the provider already active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalApply {
    /// `/connect`: adopt `provider` as the preferred provider and pin `model` to it.
    Provider { provider: String, model: String },
    /// `/models`: pin `model` to the already-active `provider`. `verified` records whether the id
    /// came from the provider's own list or was typed blind, which is what the status line says.
    Model {
        provider: String,
        model: String,
        verified: bool,
    },
}

/// Which `UiEvent` a spawned model-list fetch reports back on.
///
/// Carried out of [`Modal::fetch_target`] by the state that asked for the fetch, so the sink is
/// decided by the same match that decided there was a fetch at all — never re-derived from `App`'s
/// state after the fact, which would be correct only for as long as nothing else had moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchSink {
    /// `UiEvent::ConnectModels` — filled into the `/connect` modal's list.
    Connect,
    /// `UiEvent::ModelsFetched` — filled into the `/models` modal's list.
    Models,
}

/// The result of stepping any modal: advance to a new state, commit a [`ModalApply`], or close.
///
/// `Apply` carries what it commits rather than naming it for the caller to look up again. That
/// makes "a step with nothing to commit cannot produce an `Apply`" structural — the rule is stated
/// once, in the transition arm that has the selection in scope, and there is no second read of the
/// modal (after the caller has already begun mutating state) that could disagree with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModalTransition {
    Step(Modal),
    Apply(ModalApply),
    Close,
}

impl Modal {
    /// Pure: maps a key press in the current state to the next state, apply, or close. No keyring,
    /// terminal, or network state.
    pub(crate) fn next(&self, key: KeyEvent) -> ModalTransition {
        match self {
            Modal::Help => help_step_next(key),
            Modal::Connect(step) => connect_step_next(step, key),
            Modal::Models(step) => models_step_next(step, key),
        }
    }

    /// The provider whose model list this state is waiting on, and where the result must be
    /// delivered. Comparing this across a transition is what starts a fetch exactly once, on the
    /// step that begins waiting.
    ///
    /// Every variant is spelled out rather than falling through a `_ => None`: a modal or step
    /// added later must say here whether it fetches, and the compiler asks. A catch-all would
    /// answer "no fetch" on its behalf, and the failure would be a list that loads forever.
    pub(crate) fn fetch_target(&self) -> Option<(&str, FetchSink)> {
        match self {
            Modal::Connect(ConnectStep::ModelList {
                provider,
                fetching: true,
                ..
            }) => Some((provider.as_str(), FetchSink::Connect)),
            Modal::Models(ModelsStep::ModelList {
                provider,
                fetching: true,
                ..
            }) => Some((provider.as_str(), FetchSink::Models)),
            Modal::Help
            | Modal::Connect(
                ConnectStep::ProviderList { .. }
                | ConnectStep::KeyEntry { .. }
                | ConnectStep::ModelList {
                    fetching: false, ..
                },
            )
            | Modal::Models(
                ModelsStep::ModelList {
                    fetching: false, ..
                }
                | ModelsStep::Manual { .. }
                | ModelsStep::Credentials { .. }
                | ModelsStep::Offline,
            ) => None,
        }
    }

    /// The i18n key for the status-bar hint this modal overrides, or `None` to leave the hint to
    /// the screen underneath. Only help takes the line over; the popups float and let the base
    /// screen keep saying what it was saying.
    pub(crate) fn hint_key(&self) -> Option<&'static str> {
        match self {
            Modal::Help => Some("hint.help_close"),
            Modal::Connect(_) | Modal::Models(_) => None,
        }
    }
}

/// Pure step-transition for the help modal. Esc and Ctrl-P close it; every other key is ignored,
/// so the screen underneath cannot be driven from behind the overlay. "Every other key" is every
/// key that reaches here — the global Ctrl-C quit is intercepted by `App::handle_modal_key` before
/// any modal's transition runs.
fn help_step_next(key: KeyEvent) -> ModalTransition {
    match key.code {
        KeyCode::Esc => ModalTransition::Close,
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            ModalTransition::Close
        }
        _ => ModalTransition::Step(Modal::Help),
    }
}

/// Owns the open modal together with the counter that invalidates its in-flight fetch.
///
/// The nonce outlives the modal on purpose: every mutation that walks away from an awaited model
/// list bumps it, so a fetch already in flight is discarded when it lands. Keeping it here — rather
/// than as a second `App` field beside an `Option<Modal>` — is what makes "abandoning a fetch
/// invalidates it" impossible to forget at a call site.
///
/// **The nonce is monotonic, and must stay so.** One counter serves every modal that has ever been
/// open, which is sound only because no value is ever reused. Resetting it to `0` on close — the
/// obvious-looking tidy-up — would let a later modal's fetch collide with an earlier one's nonce
/// and accept its result.
///
/// **One invalidation seam.** [`Self::invalidate_fetch`] is the only place the nonce moves and the
/// only place the task is aborted, and every mutator that abandons an in-flight fetch routes
/// through it: [`Self::open`], [`Self::close`], [`Self::replace_step`] when the awaited target
/// changes, and [`Self::next_fetch_nonce`]. **Abort wherever the nonce is bumped** — the two are
/// one event, and anything else that must happen when a fetch is abandoned belongs in that method
/// rather than at the four call sites.
///
/// The `replace_step` site is not optional. `ProviderList → Enter → ModelList{fetching}` spawns a
/// task, `Esc → ProviderList` steps back through neither `open` nor `close`, and `Enter` again
/// spawns a second — leaving the first running to completion, holding a connection whose headers
/// carry the provider API key.
#[derive(Default)]
pub(crate) struct ModalHost {
    open: Option<Modal>,
    nonce: u64,
    /// Cancellation handle for the in-flight model fetch — **not** an "is a fetch in flight"
    /// predicate. It is `Some` from the spawn until whichever comes first: an abort, or the result
    /// landing in a `handle_*` fill. A task that has already sent its `UiEvent` but not yet been
    /// polled to completion still reads as `Some`, and `abort()` on it is a no-op.
    fetch: Option<tokio::task::JoinHandle<()>>,
}

impl ModalHost {
    /// Abandon whatever fetch this host is awaiting: cancel the task and bump the nonce so a result
    /// already in flight is discarded when it lands. The one seam — see the type docs.
    ///
    /// The abort and the nonce bump *complement* each other rather than one replacing the other:
    /// cancellation lands at the task's next await point, so a task that already posted its
    /// `UiEvent` still delivers it, and the nonce is what discards that.
    fn invalidate_fetch(&mut self) {
        if let Some(task) = self.fetch.take() {
            task.abort();
        }
        self.nonce += 1;
    }

    /// Track the task just spawned for the nonce [`Self::next_fetch_nonce`] handed out, so a later
    /// invalidation can cancel it.
    pub(crate) fn track_fetch(&mut self, task: tokio::task::JoinHandle<()>) {
        self.fetch = Some(task);
    }

    /// Whether a fetch task is still tracked as cancellable. Test-only, and deliberately *not* an
    /// "is a fetch in flight" predicate — see the [`Self::fetch`] field.
    #[cfg(test)]
    pub(crate) fn tracks_fetch(&self) -> bool {
        self.fetch.is_some()
    }

    /// Stop tracking a fetch that has delivered its own result. Not an abort: the task is done, and
    /// leaving a finished handle in place invites a future reader to treat `Some` as "already
    /// fetching".
    pub(crate) fn forget_fetch(&mut self) {
        self.fetch = None;
    }

    /// Open `modal`, replacing whatever was open and invalidating its in-flight fetch.
    pub(crate) fn open(&mut self, modal: Modal) {
        self.invalidate_fetch();
        self.open = Some(modal);
    }

    /// Close whatever is open and invalidate its in-flight fetch.
    ///
    /// Unconditional on purpose. A fetch outlives the modal that started it — `Apply` closes a
    /// connect list whose request may still be in flight — so "nothing is open" is not evidence
    /// that nothing needs invalidating. Bumping with nothing open costs one increment and keeps
    /// this a plain fetch-generation counter rather than a modal-state-conditional one.
    pub(crate) fn close(&mut self) {
        self.open = None;
        self.invalidate_fetch();
    }

    /// Swap the open modal's state, invalidating the fetch it was awaiting only if the new state is
    /// no longer waiting on the same thing.
    ///
    /// A step that keeps the same [`Modal::fetch_target`] — moving the cursor inside a list that is
    /// still loading — must keep its result, which is why this is not an unconditional bump. A step
    /// that walks away from one — Esc out of a fetching list, back to the provider list — must not,
    /// which is why it is not an unconditional skip either. Without the bump the abandoned result
    /// is stopped only by the receiver-side shape guard, a second copy of the rule maintained
    /// somewhere else.
    pub(crate) fn replace_step(&mut self, modal: Modal) {
        let target_changed =
            self.open.as_ref().and_then(|m| m.fetch_target()) != modal.fetch_target();
        if target_changed {
            self.invalidate_fetch();
        }
        self.open = Some(modal);
    }

    pub(crate) fn current(&self) -> Option<&Modal> {
        self.open.as_ref()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut Modal> {
        self.open.as_mut()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub(crate) fn nonce(&self) -> u64 {
        self.nonce
    }

    /// Claim a nonce for a newly-spawned fetch, invalidating any earlier one. The returned value is
    /// what that fetch's result must carry back to be accepted.
    pub(crate) fn next_fetch_nonce(&mut self) -> u64 {
        self.invalidate_fetch();
        self.nonce
    }
}

/// Move a list selection up (`-1`) or down (`+1`), wrapping at the ends.
fn cycle_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    if next < 0 {
        len - 1
    } else if next >= len as isize {
        0
    } else {
        next as usize
    }
}

/// Pure step-transition for the connect modal: maps a key press in the current step to the next
/// step (or close). No keyring, terminal, or network state — the `rows` carried in each step make
/// "back" navigation total.
fn connect_step_next(step: &ConnectStep, key: KeyEvent) -> ModalTransition {
    match step {
        ConnectStep::ProviderList { rows, selected } => match key.code {
            KeyCode::Esc => ModalTransition::Close,
            KeyCode::Up => ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                rows: rows.clone(),
                selected: cycle_index(*selected, rows.len(), -1),
            })),
            KeyCode::Down => ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                rows: rows.clone(),
                selected: cycle_index(*selected, rows.len(), 1),
            })),
            KeyCode::Enter => match rows.get(*selected) {
                // `Unavailable` proceeds like `Present`: the fetch re-reads the store and reports
                // the real failure, where key entry would ask for a key the user may already have
                // stored and then fail to write it to the same store.
                Some(row)
                    if matches!(row.key, RowKey::Present | RowKey::Unavailable)
                        || row.id == "ollama" =>
                {
                    ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                        rows: rows.clone(),
                        provider: row.id.clone(),
                        models: Vec::new(),
                        selected: 0,
                        fetching: true,
                        error: None,
                        from_key: false,
                    }))
                }
                Some(row) => ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry {
                    rows: rows.clone(),
                    provider: row.id.clone(),
                    input: String::new(),
                })),
                None => ModalTransition::Step(Modal::Connect(step.clone())),
            },
            _ => ModalTransition::Step(Modal::Connect(step.clone())),
        },
        ConnectStep::KeyEntry {
            rows,
            provider,
            input,
        } => match key.code {
            KeyCode::Esc => ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                rows: rows.clone(),
                selected: rows.iter().position(|r| r.id == *provider).unwrap_or(0),
            })),
            KeyCode::Enter if !input.trim().is_empty() => {
                ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                    rows: rows.clone(),
                    provider: provider.clone(),
                    models: Vec::new(),
                    selected: 0,
                    fetching: true,
                    error: None,
                    from_key: true,
                }))
            }
            KeyCode::Backspace => {
                let mut next = input.clone();
                next.pop();
                ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry {
                    rows: rows.clone(),
                    provider: provider.clone(),
                    input: next,
                }))
            }
            KeyCode::Char(c) => ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry {
                rows: rows.clone(),
                provider: provider.clone(),
                input: format!("{input}{c}"),
            })),
            _ => ModalTransition::Step(Modal::Connect(step.clone())),
        },
        ConnectStep::ModelList {
            rows,
            provider,
            models,
            selected,
            fetching,
            error,
            from_key,
        } => match key.code {
            KeyCode::Esc => {
                if !*fetching && takes_key(provider) && (*from_key || error.is_some()) {
                    ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry {
                        rows: rows.clone(),
                        provider: provider.clone(),
                        input: String::new(),
                    }))
                } else {
                    ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                        rows: rows.clone(),
                        selected: rows.iter().position(|r| r.id == *provider).unwrap_or(0),
                    }))
                }
            }
            // `models.get` subsumes the emptiness check and hands the payload straight to `Apply`,
            // so there is no state in which this returns `Apply` with nothing to commit.
            KeyCode::Enter if !*fetching => match models.get(*selected) {
                Some(model) => ModalTransition::Apply(ModalApply::Provider {
                    provider: provider.clone(),
                    model: model.clone(),
                }),
                None => ModalTransition::Step(Modal::Connect(step.clone())),
            },
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                    rows: rows.clone(),
                    provider: provider.clone(),
                    models: models.clone(),
                    selected: cycle_index(*selected, models.len(), delta),
                    fetching: *fetching,
                    error: error.clone(),
                    from_key: *from_key,
                }))
            }
            _ => ModalTransition::Step(Modal::Connect(step.clone())),
        },
    }
}

/// Map an HTTP status to a failure class. Only 401 and 403 unambiguously mean "the credential you
/// sent was refused"; 429 is a rate limit a retry genuinely fixes, and guessing at 400 would
/// misroute real bad-request bugs into a step with no retry.
fn class_for_status(status: Option<u16>) -> FetchFailure {
    match status {
        Some(401) | Some(403) => FetchFailure::Auth,
        _ => FetchFailure::Fetch,
    }
}

/// Classify a model-list fetch error. `list_models` returns an untyped `anyhow::Error`, so the
/// status is recovered by walking the source chain for the underlying `reqwest::Error` — walking
/// the whole chain rather than downcasting the root keeps this correct if a caller adds context.
/// An unrecognised error degrades to [`FetchFailure::Fetch`], which is the pre-existing behaviour.
fn classify_fetch_error(err: &anyhow::Error) -> FetchFailure {
    let status = err
        .chain()
        .find_map(|e| e.downcast_ref::<reqwest::Error>())
        .and_then(reqwest::Error::status)
        .map(|s| s.as_u16());
    class_for_status(status)
}

/// The failure class for a fetch error, given which provider produced it.
///
/// Ollama takes no key, so no failure of its is repairable by `/connect` or `/key` — not even a
/// 401 from a proxy in front of it. Forcing the transport class keeps the modal from suggesting a
/// remedy that does not exist for this provider.
fn class_for_provider(provider: &str, err: &anyhow::Error) -> FetchFailure {
    if provider == "ollama" {
        FetchFailure::Fetch
    } else {
        classify_fetch_error(err)
    }
}

/// The longest provider-supplied error text that may reach a rendered line.
const PROVIDER_ERROR_MAX_CHARS: usize = 120;

/// Reduce a provider-supplied error to one bounded, control-free line before it becomes
/// user-visible text.
///
/// The text is remote-controlled and unbounded: serde's `invalid_type` embeds the entire offending
/// value, and a hostile endpoint can answer with pages of newline-separated prose. Rendered as-is
/// it fills the modal body and pushes the modal's own trusted rows — the remedy, and on the manual
/// step the input box — past the bottom of the screen, which turns a model picker into a
/// credential-phishing surface. Control characters go too: a raw `ESC` written into a terminal
/// cell is an escape-sequence injection. The two rules it composes live in [`crate::text`] so the
/// credential-store path can share them.
fn summarize_provider_error(message: &str) -> String {
    crate::text::truncate_chars(&crate::text::one_line(message), PROVIDER_ERROR_MAX_CHARS)
}

/// Classify a provider's fetch error and bound its text. The single place remote error text
/// crosses into the UI, so the cap cannot be bypassed by a new caller.
pub(crate) fn fetch_error(provider: &str, err: &anyhow::Error) -> FetchError {
    FetchError {
        class: class_for_provider(provider, err),
        // `{:#}` keeps anyhow's source chain on one line — `to_string` reports only the outermost
        // message, which hides the actual cause (connection refused, DNS failure, TLS, 401, ...).
        message: summarize_provider_error(&format!("{err:#}")),
    }
}

/// Fetch a provider's model list off the UI loop. `key_override` is a just-typed key that wins
/// over the stored one; otherwise the key is resolved from the environment or the keyring. The key
/// is consumed here and never returned to the caller.
/// Resolve a key for `provider` and fetch its model ids, reporting a panic inside that work as an
/// error instead of as silence.
///
/// Nothing polls the fetch task's `JoinHandle` — the event loop is driven by `UiEvent`s, not by
/// task completion — so an unwind inside the fetch (`resolve_key` reaching the OS keyring is the
/// plausible candidate) would send no `UiEvent` at all, leaving `fetching: true` and the modal on
/// "Fetching models..." until Esc. The 15s deadline lives inside reqwest, around the request, not
/// around the task, so it does not rescue this case.
pub(crate) async fn fetch_model_list(
    provider: &str,
    key_override: Option<String>,
    store: &dyn CredentialStore,
    locale: Locale,
) -> Result<Vec<String>, FetchError> {
    guard_panic(
        fetch_model_list_inner(provider, key_override, store, locale),
        locale,
    )
    .await
}

/// Turn a panic inside `fut` into a reportable error.
///
/// `AssertUnwindSafe` is sound here because nothing observable survives the unwind: the caller is a
/// spawned task that ends either way, its locals drop, and only this error string escapes. The
/// default panic hook still prints the panic, so the unwind is not swallowed silently.
///
/// A panic is classified `Fetch`, not a credential failure: it says nothing about the key, and
/// #47's rule is that an unrecognised failure degrades to the retryable class rather than to a
/// step that offers a remedy which cannot help.
async fn guard_panic<F>(fut: F, locale: Locale) -> Result<Vec<String>, FetchError>
where
    F: std::future::Future<Output = Result<Vec<String>, FetchError>>,
{
    std::panic::AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(FetchError {
                class: FetchFailure::Fetch,
                message: i18n::t(locale, "connect.fetch_panicked").to_string(),
            })
        })
}

async fn fetch_model_list_inner(
    provider: &str,
    key_override: Option<String>,
    store: &dyn CredentialStore,
    locale: Locale,
) -> Result<Vec<String>, FetchError> {
    if provider == "ollama" {
        return list_ollama_models()
            .await
            .map_err(|e| fetch_error(provider, &e));
    }
    let key = match key_override {
        Some(k) => Some(k),
        None => match crate::selection::resolve_key(provider, store) {
            crate::selection::KeyResolution::Found(k) => Some(k),
            crate::selection::KeyResolution::Missing => None,
            crate::selection::KeyResolution::Unavailable(error) => {
                return Err(FetchError {
                    class: FetchFailure::StoreUnavailable,
                    // Our own sentence rather than a remote one, but still capped: the cap is
                    // what keeps the modal's own remedy rows on screen when the backend is
                    // verbose.
                    message: summarize_provider_error(&i18n::t_with(
                        locale,
                        "connect.store_unavailable",
                        &[("provider", provider), ("error", &error)],
                    )),
                });
            }
        },
    };
    fetch_with_key(provider, key, locale).await
}

/// The classification boundary: an already-resolved key (or the absence of one) becomes either a
/// model list or a classified [`FetchError`].
///
/// Split out from [`fetch_model_list`] so the no-key arm is reachable from a test without mutating
/// the process environment — `resolve_key` reads `OPENAI_API_KEY` and friends, so a developer with
/// one exported would otherwise never execute this branch.
async fn fetch_with_key(
    provider: &str,
    key: Option<String>,
    locale: Locale,
) -> Result<Vec<String>, FetchError> {
    match key {
        Some(k) => list_models(provider, &k)
            .await
            .map_err(|e| fetch_error(provider, &e)),
        // Our own sentence, not the provider's: it is not summarized, and not capped.
        None => Err(FetchError {
            class: FetchFailure::MissingKey,
            message: i18n::t_with(locale, "connect.no_key", &[("provider", provider)]),
        }),
    }
}

/// Pure step-transition for the models modal: maps a key press in the current step to the next
/// step, apply, or close. No network/keyring/terminal state.
fn models_step_next(step: &ModelsStep, key: KeyEvent) -> ModalTransition {
    match step {
        // Nothing to retry: `Offline` is reached before a provider is chosen, so there is no
        // fetch to re-run.
        ModelsStep::Offline => match key.code {
            KeyCode::Esc | KeyCode::Enter => ModalTransition::Close,
            _ => ModalTransition::Step(Modal::Models(step.clone())),
        },
        // A 401/403 does not always mean the API key is wrong — a corporate proxy, a WAF, an IP
        // allowlist, or an org-level block produce the same status, and for those `/connect` and
        // `/key` are as useless as the retry-only modal #47 replaced. Keeping the retry means a
        // misclassification costs a keystroke rather than dead-ending the user.
        ModelsStep::Credentials { provider, .. } => match key.code {
            KeyCode::Char('r' | 'R') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                refetch(provider)
            }
            KeyCode::Esc | KeyCode::Enter => ModalTransition::Close,
            _ => ModalTransition::Step(Modal::Models(step.clone())),
        },
        ModelsStep::ModelList {
            provider,
            models,
            selected,
            fetching,
        } => {
            if *fetching {
                match key.code {
                    KeyCode::Esc => ModalTransition::Close,
                    _ => ModalTransition::Step(Modal::Models(step.clone())),
                }
            } else if models.is_empty() {
                // A successful fetch can still return nothing (an Ollama install with no models
                // pulled). That is worth another try, so offer the same retry the failure steps do.
                match key.code {
                    KeyCode::Esc => ModalTransition::Close,
                    KeyCode::Char('r' | 'R') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        refetch(provider)
                    }
                    _ => ModalTransition::Step(Modal::Models(step.clone())),
                }
            } else {
                match key.code {
                    KeyCode::Esc => ModalTransition::Close,
                    KeyCode::Enter => match models.get(*selected) {
                        Some(model) => ModalTransition::Apply(ModalApply::Model {
                            provider: provider.clone(),
                            model: model.clone(),
                            verified: true,
                        }),
                        None => ModalTransition::Step(Modal::Models(step.clone())),
                    },
                    KeyCode::Up | KeyCode::Down => {
                        let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                        ModalTransition::Step(Modal::Models(ModelsStep::ModelList {
                            provider: provider.clone(),
                            models: models.clone(),
                            selected: cycle_index(*selected, models.len(), delta),
                            fetching: false,
                        }))
                    }
                    _ => ModalTransition::Step(Modal::Models(step.clone())),
                }
            }
        }
        ModelsStep::Manual {
            provider,
            input,
            error,
        } => match key.code {
            KeyCode::Esc => ModalTransition::Close,
            // Ordered before the `Char(c)` arm below, which would otherwise type the `r`. `'R'`
            // is matched too: Ctrl+Shift+R arrives as `Char('R')` with CONTROL|SHIFT and would
            // otherwise fall through and type an `R` into the model id.
            KeyCode::Char('r' | 'R') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                refetch(provider)
            }
            KeyCode::Enter if !input.trim().is_empty() => {
                ModalTransition::Apply(ModalApply::Model {
                    provider: provider.clone(),
                    model: input.trim().to_string(),
                    // Typed blind: nothing has confirmed the provider serves this id.
                    verified: false,
                })
            }
            KeyCode::Backspace => {
                let mut next = input.clone();
                next.pop();
                ModalTransition::Step(Modal::Models(ModelsStep::Manual {
                    provider: provider.clone(),
                    input: next,
                    error: error.clone(),
                }))
            }
            KeyCode::Char(c) => ModalTransition::Step(Modal::Models(ModelsStep::Manual {
                provider: provider.clone(),
                input: format!("{input}{c}"),
                error: error.clone(),
            })),
            _ => ModalTransition::Step(Modal::Models(step.clone())),
        },
    }
}

/// Re-run `provider`'s model-list fetch: a plain step back to an empty, fetching list.
///
/// This needs no `Retry` transition of its own. `handle_modal_key` starts a fetch whenever
/// [`Modal::fetch_target`] goes `None -> Some` across a transition, and every step that offers a
/// retry has `None` as its target, so the step *is* the retry. `ModalHost::replace_step` bumps the
/// nonce on the same target change, which is what discards a still-in-flight earlier result.
fn refetch(provider: &str) -> ModalTransition {
    ModalTransition::Step(Modal::Models(ModelsStep::ModelList {
        provider: provider.to_string(),
        models: Vec::new(),
        selected: 0,
        fetching: true,
    }))
}

/// Assemble the help modal body for `locale`: section headers followed by their indented entries,
/// with a blank line between sections. Pure and unit-testable (no ratatui types).
fn help_lines(locale: Locale) -> Vec<String> {
    const SECTIONS: &[(&str, &[&str])] = &[
        (
            "help.section.global",
            &["help.global.help", "help.global.quit"],
        ),
        (
            "help.section.forms",
            &[
                "help.forms.navigate",
                "help.forms.submit",
                "help.forms.command",
                "help.forms.back",
            ],
        ),
        (
            "help.section.connected",
            &[
                "help.connected.ping",
                "help.connected.signout",
                "help.connected.engine",
                "help.connected.quit",
            ],
        ),
        (
            "help.section.engine",
            &[
                "help.engine.send",
                "help.engine.back",
                "help.engine.approve",
            ],
        ),
        (
            "help.section.commands",
            &[
                "help.commands.ask",
                "help.commands.connect",
                "help.commands.model",
                "help.commands.models",
                "help.commands.key",
                "help.commands.auth",
                "help.commands.lang",
            ],
        ),
    ];

    let mut lines = Vec::new();
    for (section, entries) in SECTIONS {
        lines.push(i18n::t(locale, section).to_string());
        for entry in *entries {
            lines.push(format!("  {}", i18n::t(locale, entry)));
        }
        lines.push(String::new());
    }
    lines
}
/// The slice of `App` a modal needs to render itself, so a view can be built from `&App` without
/// borrowing the terminal.
pub(crate) struct ModalContext<'a> {
    pub(crate) locale: Locale,
    /// The app-level error line, rendered inside the connect key-entry step.
    pub(crate) error: Option<&'a str>,
    /// Why the active provider is offline, rendered by the models modal's offline step.
    pub(crate) offline: Option<&'a OfflineReason>,
}

/// A modal rendered as a centered popup over the screen underneath it.
pub(crate) struct PopupView {
    pub(crate) title: String,
    pub(crate) body: Vec<Line<'static>>,
    pub(crate) footer: String,
    /// A `body` row that must stay on screen; [`draw_popup`] scrolls to keep it visible, counting
    /// the wrapped rows each earlier line occupies rather than the logical lines (#57).
    pub(crate) focus: Option<usize>,
}

/// A modal rendered as a full-area pane, replacing the screen underneath it.
pub(crate) struct FullScreenView {
    pub(crate) title: String,
    pub(crate) body: Vec<Line<'static>>,
}

pub(crate) enum ModalView {
    Popup(PopupView),
    FullScreen(FullScreenView),
}

impl ModalView {
    /// Whether this modal replaces the screen underneath it, or floats over it.
    ///
    /// Read off the view rather than stated separately per modal, because it is a property of how
    /// the modal is *painted*, not of which modal it is: [`draw_full_screen`] deliberately does not
    /// `Clear`, so a modal that claimed to cover the base while rendering a `Popup` would paint
    /// over a screen nobody drew.
    pub(crate) fn covers_base(&self) -> bool {
        matches!(self, ModalView::FullScreen(_))
    }
}

impl Modal {
    /// This modal's rendering, decoupled from the frame. The one place a modal's body, footer and
    /// focus are decided; [`draw_modal`] is the one place they are painted.
    pub(crate) fn view(&self, ctx: &ModalContext<'_>) -> ModalView {
        match self {
            Modal::Help => ModalView::FullScreen(FullScreenView {
                title: i18n::t(ctx.locale, "title.help").to_string(),
                body: help_lines(ctx.locale).into_iter().map(Line::from).collect(),
            }),
            Modal::Connect(step) => ModalView::Popup(connect_view(step, ctx)),
            Modal::Models(step) => ModalView::Popup(models_view(step, ctx)),
        }
    }
}

/// The connect modal's rendering.
fn connect_view(step: &ConnectStep, ctx: &ModalContext<'_>) -> PopupView {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let title: String;
    match step {
        ConnectStep::ProviderList { rows, selected } => {
            title = i18n::t(ctx.locale, "connect.title").to_string();
            for (i, row) in rows.iter().enumerate() {
                let marker = if i == *selected { "> " } else { "  " };
                let style = if i == *selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };
                let suffix = match row.key {
                    RowKey::Present => format!(" ({})", i18n::t(ctx.locale, "connect.connected")),
                    RowKey::Absent => String::new(),
                    RowKey::Unavailable => format!(
                        " ({})",
                        i18n::t(ctx.locale, "connect.store_unavailable_row")
                    ),
                };
                lines.push(Line::from(Span::styled(
                    format!("{marker}{}{suffix}", row.id),
                    style,
                )));
            }
        }
        ConnectStep::KeyEntry {
            provider, input, ..
        } => {
            title = i18n::t(ctx.locale, "connect.key_heading").to_string();
            lines.push(Line::from(i18n::t_with(
                ctx.locale,
                "status.key_enter",
                &[("provider", provider)],
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                mask(input),
                Style::default().add_modifier(Modifier::REVERSED),
            )));
            if let Some(err) = ctx.error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    err.to_string(),
                    Style::default().fg(Color::Red),
                )));
            }
        }
        ConnectStep::ModelList {
            provider,
            models,
            selected,
            fetching,
            error,
            ..
        } => {
            title = i18n::t_with(
                ctx.locale,
                "connect.models_heading",
                &[("provider", provider)],
            );
            if *fetching {
                lines.push(Line::from(Span::styled(
                    i18n::t(ctx.locale, "connect.fetching"),
                    Style::default().fg(Color::DarkGray),
                )));
            } else if let Some(err) = error {
                lines.push(Line::from(Span::styled(
                    err.clone(),
                    Style::default().fg(Color::Red),
                )));
            } else if models.is_empty() {
                lines.push(Line::from(Span::styled(
                    i18n::t(ctx.locale, "connect.no_models"),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for (i, model) in models.iter().enumerate() {
                    let marker = if i == *selected { "> " } else { "  " };
                    let style = if i == *selected {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(Span::styled(format!("{marker}{model}"), style)));
                }
            }
        }
    }

    let footer = match step {
        ConnectStep::ProviderList { .. } => i18n::t(ctx.locale, "connect.footer_list"),
        ConnectStep::KeyEntry { .. } => i18n::t(ctx.locale, "connect.footer_key"),
        ConnectStep::ModelList { fetching, .. } => {
            if *fetching {
                i18n::t(ctx.locale, "connect.footer_fetching")
            } else {
                i18n::t(ctx.locale, "connect.footer_models")
            }
        }
    };
    let focus = match step {
        ConnectStep::ProviderList { selected, .. } => Some(*selected),
        ConnectStep::ModelList {
            selected,
            fetching: false,
            models,
            ..
        } if !models.is_empty() => Some(*selected),
        _ => None,
    };
    PopupView {
        title,
        body: lines,
        footer: footer.to_string(),
        focus,
    }
}

/// The models modal's rendering.
fn models_view(step: &ModelsStep, ctx: &ModalContext<'_>) -> PopupView {
    let title = i18n::t(ctx.locale, "models.title").to_string();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let footer: &str;
    match step {
        ModelsStep::Offline => {
            if let Some(reason) = &ctx.offline {
                lines.push(Line::from(Span::styled(
                    crate::provider::offline_notice(ctx.locale, reason),
                    Style::default().fg(Color::Yellow),
                )));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                i18n::t(ctx.locale, "models.offline"),
                Style::default().fg(Color::DarkGray),
            )));
            footer = i18n::t(ctx.locale, "models.footer_offline");
        }
        ModelsStep::ModelList {
            models,
            selected,
            fetching,
            ..
        } => {
            if *fetching {
                lines.push(Line::from(Span::styled(
                    i18n::t(ctx.locale, "connect.fetching"),
                    Style::default().fg(Color::DarkGray),
                )));
                footer = i18n::t(ctx.locale, "connect.footer_fetching");
            } else if models.is_empty() {
                lines.push(Line::from(Span::styled(
                    i18n::t(ctx.locale, "connect.no_models"),
                    Style::default().fg(Color::DarkGray),
                )));
                // Enter is a no-op with nothing to select, so don't advertise it. A retry is
                // not: an empty list is reachable from an Ollama install with nothing pulled.
                footer = i18n::t(ctx.locale, "models.footer_retry");
            } else {
                for (i, model) in models.iter().enumerate() {
                    let marker = if i == *selected { "> " } else { "  " };
                    let style = if i == *selected {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(Span::styled(format!("{marker}{model}"), style)));
                }
                footer = i18n::t(ctx.locale, "models.footer_list");
            }
        }
        // Trusted rows first, provider text last. `draw_popup` sizes itself from the wrapped row
        // count, so nothing should be clipped — but if a very short terminal clips anyway, what
        // survives must be the remedy rather than the remote-supplied error that would otherwise
        // have displaced it.
        ModelsStep::Credentials {
            provider,
            error,
            class,
        } => {
            lines.push(Line::from(Span::styled(
                i18n::t(ctx.locale, "models.credentials_hint"),
                Style::default().fg(Color::DarkGray),
            )));
            if let Some(key) = class.remedy_key() {
                lines.push(Line::from(Span::styled(
                    i18n::t_with(ctx.locale, key, &[("provider", provider)]),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )));
            // A 401/403 is not always about the key — a corporate proxy, a WAF, or an IP
            // allowlist produces the same status — and a store failure can be a transient D-Bus
            // blip, so the step keeps a retry rather than dead-ending on a remedy.
            footer = i18n::t(ctx.locale, "models.footer_retry");
        }
        ModelsStep::Manual {
            provider,
            input,
            error,
        } => {
            lines.push(Line::from(Span::styled(
                i18n::t_with(
                    ctx.locale,
                    "models.manual_unverified",
                    &[("provider", provider)],
                ),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                input.clone(),
                Style::default().add_modifier(Modifier::REVERSED),
            )));
            if let Some(err) = error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    err.clone(),
                    Style::default().fg(Color::Red),
                )));
            }
            footer = i18n::t(ctx.locale, "models.footer_manual");
        }
    }
    let focus = match step {
        ModelsStep::ModelList {
            selected,
            fetching: false,
            models,
            ..
        } if !models.is_empty() => Some(*selected),
        _ => None,
    };
    PopupView {
        title,
        body: lines,
        footer: footer.to_string(),
        focus,
    }
}

/// Paint a modal. The single seam every modal's rendering passes through.
pub(crate) fn draw_modal(frame: &mut Frame, area: Rect, view: ModalView) {
    match view {
        ModalView::Popup(view) => draw_popup(frame, area, view),
        ModalView::FullScreen(view) => draw_full_screen(frame, area, view),
    }
}

/// Render a full-area modal pane. Unlike [`draw_popup`] this does not clear behind itself, because
/// the caller does not draw the screen underneath a modal that covers it.
fn draw_full_screen(frame: &mut Frame, area: Rect, view: FullScreenView) {
    let pane = centered_rect(80, 90, area);
    let paragraph = Paragraph::new(view.body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", view.title)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, pane);
}

/// The rows `line` occupies once wrapped to `width`, measured with the very wrapper the render
/// below uses, so the two cannot disagree.
fn wrapped_rows(line: &Line, width: u16) -> usize {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1)
}

/// Render a centered, bordered popup, clearing what is underneath. The footer is pinned to the
/// bottom so it stays visible, and `view.focus` names a body row that must remain on screen — the
/// body scrolls to keep it visible when the list is taller than the terminal.
///
/// The box is sized from the **wrapped** row count, not from `body.len()`. Sizing from the logical
/// line count silently clipped every body line a provider-supplied string pushed past the 58-column
/// inner width: `focus` is `None` on the notice steps, so the scroll offset is 0 and the overflow
/// is never reachable. On the `/models` credential step that took the remedy off screen, and on the
/// manual step it took the input box off screen while keystrokes still accumulated (#57).
///
/// The one place a popup's geometry is decided, reached from the one call site in [`draw_modal`].
fn draw_popup(frame: &mut Frame, area: Rect, view: PopupView) {
    let PopupView {
        title,
        body,
        footer,
        focus,
    } = view;
    let footer = Line::from(Span::styled(footer, Style::default().fg(Color::DarkGray)));
    // Borders take two rows and the pinned footer one.
    const CHROME: u16 = 3;
    let available = area.height.saturating_sub(2);
    let width = 60u16.min(area.width.saturating_sub(2));
    // The block's left and right borders each take a column.
    let inner_width = width.saturating_sub(2);
    let rows: Vec<usize> = body.iter().map(|l| wrapped_rows(l, inner_width)).collect();
    // Clamp in `usize` first: a remote-supplied list long enough to overflow `u16` must not wrap.
    let wanted = rows
        .iter()
        .copied()
        .fold(0usize, usize::saturating_add)
        .saturating_add(CHROME as usize);
    let height = u16::try_from(wanted).unwrap_or(u16::MAX).min(available);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", title));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let body_height = inner.height.saturating_sub(1);
    if body_height > 0 {
        // Scroll just far enough to bring the focused row into view. `scroll` counts *wrapped*
        // rows, so the focused body line's position is summed in wrapped rows too.
        let offset = match focus {
            Some(row) => {
                let end = rows
                    .iter()
                    .take(row.saturating_add(1))
                    .copied()
                    .fold(0usize, usize::saturating_add);
                u16::try_from(end)
                    .unwrap_or(u16::MAX)
                    .saturating_sub(body_height)
            }
            None => 0,
        };
        frame.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            Rect {
                height: body_height,
                ..inner
            },
        );
    }
    frame.render_widget(
        Paragraph::new(footer),
        Rect {
            y: inner.y + body_height,
            height: 1,
            ..inner
        },
    );
}

/// Center a rectangle of `percent_x` by `percent_y` within `area` (a standard ratatui modal
/// helper).
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use light_factory_tui::i18n::Locale;
    use ratatui::text::Line;
    use ratatui::{Frame, Terminal};

    use super::*;

    /// A minimal render context: no app-level error, no offline reason.
    fn ctx(locale: Locale) -> ModalContext<'static> {
        ModalContext {
            locale,
            error: None,
            offline: None,
        }
    }

    /// Every rendered body row, flattened to one string.
    fn body_text(view: &PopupView) -> String {
        view.body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn model_apply(provider: &str, model: &str, verified: bool) -> ModalTransition {
        ModalTransition::Apply(ModalApply::Model {
            provider: provider.to_string(),
            model: model.to_string(),
            verified,
        })
    }

    /// What a retry looks like now that it is a plain step rather than its own transition.
    fn refetch_step(provider: &str) -> ModalTransition {
        ModalTransition::Step(Modal::Models(ModelsStep::ModelList {
            provider: provider.to_string(),
            models: Vec::new(),
            selected: 0,
            fetching: true,
        }))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn row(id: &str, key: RowKey) -> ProviderRow {
        ProviderRow {
            id: id.to_string(),
            key,
        }
    }

    fn model_list_step(models: Vec<String>, fetching: bool) -> ConnectStep {
        ConnectStep::ModelList {
            rows: vec![],
            provider: "openai".to_string(),
            models,
            selected: 0,
            fetching,
            error: None,
            from_key: false,
        }
    }

    fn models_list_step(models: Vec<String>, fetching: bool) -> ModelsStep {
        ModelsStep::ModelList {
            provider: "openai".to_string(),
            models,
            selected: 0,
            fetching,
        }
    }

    /// A manual step shaped the way production produces one: with an error present.
    /// `handle_models_fetched`'s `Err` arm is `Manual`'s only constructor and it always sets
    /// `Some(_)`, so `error: None` was a state no code path could reach — and a render test built
    /// on it asserted against fiction while dropping the very line that broke the layout.
    fn models_manual_step(input: &str) -> ModelsStep {
        ModelsStep::Manual {
            provider: "openai".to_string(),
            input: input.to_string(),
            error: Some("Couldn't fetch models: connection refused".to_string()),
        }
    }

    /// Draw `f` to an off-screen terminal and return the buffer as text, so rendering can be
    /// asserted without a real terminal.
    fn draw_to_text(width: u16, height: u16, f: impl FnOnce(&mut Frame)) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(f).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The client every network test in this module uses.
    ///
    /// `reqwest::get` builds a default client, which honours `http_proxy`/`HTTP_PROXY` and has no
    /// timeout at all. Under an exported proxy the 401/403 mocks were observed returning 200 —
    /// `expect_err` then failed — and against a sandbox that DROPs rather than RSTs the connection
    /// to port 1, the transport test blocked on the kernel SYN-retry budget (~130s) with nothing to
    /// bound it. Both are properties of the client, so both are fixed on the client.
    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("test HTTP client")
    }

    /// A real `reqwest::Error` carrying `code`, produced the way a provider produces one.
    async fn status_error(code: u16) -> anyhow::Error {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(code))
            .mount(&server)
            .await;
        test_client()
            .get(server.uri())
            .send()
            .await
            .expect("the request reached the mock")
            .error_for_status()
            .expect_err("the mock returned an error status")
            .into()
    }

    /// A real `reqwest::Error` with no HTTP status: a refused connection.
    async fn transport_error() -> anyhow::Error {
        test_client()
            .get("http://127.0.0.1:1/models")
            .send()
            .await
            .expect_err("nothing listens on port 1")
            .into()
    }

    #[test]
    fn mask_never_echoes_input() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("abc"), "***");
        assert_eq!(mask("sk-secret"), "*********");
    }

    #[test]
    fn cycle_index_wraps_at_both_ends() {
        assert_eq!(cycle_index(0, 3, -1), 2);
        assert_eq!(cycle_index(2, 3, 1), 0);
        assert_eq!(cycle_index(1, 3, 1), 2);
        assert_eq!(cycle_index(0, 0, 1), 0);
    }

    #[test]
    fn connect_provider_enter_routes_by_connection_state() {
        let rows = vec![
            row("openai", RowKey::Absent),
            row("ollama", RowKey::Present),
            row("gemini", RowKey::Present),
        ];
        let step = ConnectStep::ProviderList {
            rows: rows.clone(),
            selected: 0,
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry { provider, .. })) if provider == "openai"
        ));
        let step = ConnectStep::ProviderList {
            rows: rows.clone(),
            selected: 1,
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                provider,
                fetching: true,
                ..
            })) if provider == "ollama"
        ));
        let step = ConnectStep::ProviderList { rows, selected: 2 };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                provider,
                fetching: true,
                from_key: false,
                ..
            })) if provider == "gemini"
        ));
    }

    /// The store could not be asked, so Enter must not route to key entry: that would demand a
    /// key the user may already have stored, and storing it would fail against the same store.
    #[test]
    fn an_unavailable_row_goes_to_the_model_list_not_key_entry() {
        let step = ConnectStep::ProviderList {
            rows: vec![ProviderRow {
                id: "openai".to_string(),
                key: RowKey::Unavailable,
            }],
            selected: 0,
        };
        let ModalTransition::Step(Modal::Connect(next)) =
            connect_step_next(&step, key(KeyCode::Enter))
        else {
            panic!("Enter must step");
        };
        assert!(
            matches!(next, ConnectStep::ModelList { .. }),
            "expected the model list, got {next:?}"
        );
    }

    #[test]
    fn an_absent_row_still_goes_to_key_entry() {
        let step = ConnectStep::ProviderList {
            rows: vec![ProviderRow {
                id: "openai".to_string(),
                key: RowKey::Absent,
            }],
            selected: 0,
        };
        let ModalTransition::Step(Modal::Connect(next)) =
            connect_step_next(&step, key(KeyCode::Enter))
        else {
            panic!("Enter must step");
        };
        assert!(
            matches!(next, ConnectStep::KeyEntry { .. }),
            "expected key entry, got {next:?}"
        );
    }

    #[test]
    fn connect_ollama_skips_the_key_step_even_when_unconnected() {
        let rows = vec![row("ollama", RowKey::Absent)];
        let step = ConnectStep::ProviderList { rows, selected: 0 };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList { provider, .. })) if provider == "ollama"
        ));
    }

    #[test]
    fn connect_esc_closes_from_provider_list() {
        let step = ConnectStep::ProviderList {
            rows: vec![row("openai", RowKey::Absent)],
            selected: 0,
        };
        assert_eq!(
            connect_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
    }

    #[test]
    fn connect_key_entry_enter_blank_stays_and_esc_returns_to_list() {
        let rows = vec![row("openai", RowKey::Absent)];
        let step = ConnectStep::KeyEntry {
            rows: rows.clone(),
            provider: "openai".to_string(),
            input: String::new(),
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry { input, .. })) if input.is_empty()
        ));
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                selected: 0,
                ..
            }))
        ));
    }

    #[test]
    fn connect_key_entry_enter_with_key_fetches_models() {
        let rows = vec![row("openai", RowKey::Absent)];
        let step = ConnectStep::KeyEntry {
            rows,
            provider: "openai".to_string(),
            input: "sk-x".to_string(),
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                provider,
                fetching: true,
                from_key: true,
                ..
            })) if provider == "openai"
        ));
    }

    #[test]
    fn connect_model_list_enter_selects_and_esc_routes_back() {
        let step = ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Absent)],
            provider: "openai".to_string(),
            models: vec!["gpt-4o".to_string()],
            selected: 0,
            fetching: false,
            error: None,
            from_key: true,
        };
        // Enter on a usable list is `Apply`, not `Close`: the modal no longer has to be
        // re-inspected after the fact to discover it had something to commit.
        assert_eq!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Apply(ModalApply::Provider {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
            })
        );

        let step = ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Absent)],
            provider: "openai".to_string(),
            models: vec![],
            selected: 0,
            fetching: false,
            error: Some("bad key".to_string()),
            from_key: false,
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Step(Modal::Connect(ConnectStep::KeyEntry { provider, .. })) if provider == "openai"
        ));

        let step = ConnectStep::ModelList {
            rows: vec![row("ollama", RowKey::Absent)],
            provider: "ollama".to_string(),
            models: vec![],
            selected: 0,
            fetching: false,
            error: Some("unreachable".to_string()),
            from_key: false,
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList { .. }))
        ));
    }

    #[test]
    fn connect_model_list_enter_is_a_noop_while_fetching() {
        let step = ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Absent)],
            provider: "openai".to_string(),
            models: vec![],
            selected: 0,
            fetching: true,
            error: None,
            from_key: false,
        };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                fetching: true,
                ..
            }))
        ));
    }

    /// The row state must reach the rendered row. The transition tests pin navigation and the i18n
    /// tests pin the strings, but nothing else connects the two — a suffix wired to the wrong key
    /// would pass both.
    #[test]
    fn the_provider_list_renders_one_suffix_per_row_state() {
        let ctx = ModalContext {
            locale: Locale::En,
            error: None,
            offline: None,
        };
        let view = connect_view(
            &ConnectStep::ProviderList {
                rows: vec![
                    row("anthropic", RowKey::Present),
                    row("openai", RowKey::Absent),
                    row("gemini", RowKey::Unavailable),
                ],
                selected: 0,
            },
            &ctx,
        );
        let body: Vec<String> = view
            .body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(body[0].contains("anthropic (connected)"), "{body:?}");
        assert_eq!(
            body[1].trim(),
            "openai",
            "an absent key adds no suffix: {body:?}"
        );
        assert!(
            body[2].contains("gemini (key store unavailable)"),
            "{body:?}"
        );
    }

    #[test]
    fn connect_up_down_wrap_the_provider_selection() {
        let rows = vec![
            row("a", RowKey::Absent),
            row("b", RowKey::Absent),
            row("c", RowKey::Absent),
        ];
        let step = ConnectStep::ProviderList { rows, selected: 0 };
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Up)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                selected: 2,
                ..
            }))
        ));
        assert!(matches!(
            connect_step_next(&step, key(KeyCode::Down)),
            ModalTransition::Step(Modal::Connect(ConnectStep::ProviderList {
                selected: 1,
                ..
            }))
        ));
    }

    /// The regression behind #57: `draw_popup` sized its box from `body.len()` — logical lines,
    /// counted before wrapping — while rendering with `Wrap`. Every body line below a wrapping one
    /// fell outside the box, and with `focus: None` the scroll offset is 0, so nothing could bring
    /// it back. Two logical lines therefore rendered as one.
    #[test]
    fn draw_popup_sizes_itself_from_wrapped_rows_not_logical_lines() {
        // ~200 columns: four rows against the 58-column inner width.
        let long = "wrap ".repeat(40);
        let screen = draw_to_text(80, 24, |frame| {
            let area = frame.area();
            draw_popup(
                frame,
                area,
                PopupView {
                    title: "Title".to_string(),
                    body: vec![Line::from(long.clone()), Line::from("TAIL-MARKER")],
                    footer: "FOOTER-MARKER".to_string(),
                    focus: None,
                },
            );
        });
        assert!(
            screen.contains("TAIL-MARKER"),
            "a line below a wrapping one was clipped out of the box:\n{screen}"
        );
        assert!(
            screen.contains("FOOTER-MARKER"),
            "the pinned footer must survive:\n{screen}"
        );
    }

    /// The box must still stop growing at the terminal, and the focused row must still be scrolled
    /// into view — now counted in wrapped rows, since that is what `Paragraph::scroll` counts.
    #[test]
    fn draw_popup_scrolls_wrapped_rows_to_keep_the_focused_line_visible() {
        let mut body: Vec<Line> = (0..40)
            .map(|i| Line::from(format!("{} row-{i:02}", "pad ".repeat(20))))
            .collect();
        body.push(Line::from("FOCUSED-ROW"));
        let focus = body.len() - 1;
        let screen = draw_to_text(80, 12, |frame| {
            let area = frame.area();
            draw_popup(
                frame,
                area,
                PopupView {
                    title: "Title".to_string(),
                    body: body.clone(),
                    footer: "FOOTER-MARKER".to_string(),
                    focus: Some(focus),
                },
            );
        });
        assert!(
            screen.contains("FOCUSED-ROW"),
            "the focused row scrolled off screen:\n{screen}"
        );
        assert!(
            screen.contains("FOOTER-MARKER"),
            "the pinned footer must survive:\n{screen}"
        );
    }

    #[test]
    fn models_offline_closes_on_esc_and_enter_and_ignores_typing() {
        let step = ModelsStep::Offline;
        assert_eq!(
            models_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Close
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Char('x'))),
            ModalTransition::Step(Modal::Models(ModelsStep::Offline))
        );
    }

    #[test]
    fn models_while_fetching_cancels_on_esc_and_ignores_enter() {
        let step = models_list_step(vec![], true);
        assert_eq!(
            models_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(step.clone()))
        );
    }

    #[test]
    fn models_empty_list_enter_is_a_noop_and_esc_closes() {
        let step = models_list_step(vec![], false);
        assert_eq!(
            models_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(step.clone()))
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
    }

    #[test]
    fn models_list_enter_applies_esc_closes_and_arrows_wrap() {
        let step = models_list_step(
            vec![
                "gpt-4o".to_string(),
                "gpt-4o-mini".to_string(),
                "o3".to_string(),
            ],
            false,
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Enter)),
            model_apply("openai", "gpt-4o", true),
            "an id picked off the provider's list is verified"
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
        assert!(matches!(
            models_step_next(&step, key(KeyCode::Up)),
            ModalTransition::Step(Modal::Models(ModelsStep::ModelList { selected: 2, .. }))
        ));
        assert!(matches!(
            models_step_next(&step, key(KeyCode::Down)),
            ModalTransition::Step(Modal::Models(ModelsStep::ModelList { selected: 1, .. }))
        ));
    }

    #[test]
    fn models_manual_edits_input_and_applies_only_a_non_blank_id() {
        let blank = models_manual_step("   ");
        assert_eq!(
            models_step_next(&blank, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(blank.clone()))
        );
        assert_eq!(
            models_step_next(&blank, key(KeyCode::Esc)),
            ModalTransition::Close
        );

        let typed = models_step_next(&models_manual_step("gpt-4"), key(KeyCode::Char('o')));
        assert!(matches!(
            &typed,
            ModalTransition::Step(Modal::Models(ModelsStep::Manual { input, .. })) if input == "gpt-4o"
        ));

        let popped = models_step_next(&models_manual_step("gpt-4o"), key(KeyCode::Backspace));
        assert!(matches!(
            &popped,
            ModalTransition::Step(Modal::Models(ModelsStep::Manual { input, .. })) if input == "gpt-4"
        ));

        assert_eq!(
            models_step_next(&models_manual_step("gpt-4o"), key(KeyCode::Enter)),
            model_apply("openai", "gpt-4o", false),
            "a typed id is never verified"
        );
    }

    /// A 401/403 is not proof the key is wrong — a corporate proxy, a WAF, or an IP allowlist
    /// produces the same status. Without a retry the step is a dead end pointing at two commands
    /// that cannot help, which is #47's own defect inverted.
    #[test]
    fn the_credentials_step_offers_a_retry() {
        let step = ModelsStep::Credentials {
            provider: "openai".to_string(),
            error: "refused".to_string(),
            class: FetchFailure::Auth,
        };
        assert_eq!(
            models_step_next(&step, ctrl_key(KeyCode::Char('r'))),
            refetch_step("openai")
        );
        assert_eq!(
            models_step_next(&step, ctrl_key(KeyCode::Char('R'))),
            refetch_step("openai"),
            "Ctrl+Shift+R arrives as an uppercase R"
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Char('r'))),
            ModalTransition::Step(Modal::Models(step.clone())),
            "an unmodified r is not a retry"
        );
    }

    /// A successful fetch can legitimately return nothing (an Ollama install with no models
    /// pulled). That is worth another try rather than a modal whose only key is Esc.
    #[test]
    fn an_empty_list_offers_a_retry() {
        let step = models_list_step(vec![], false);
        assert_eq!(
            models_step_next(&step, ctrl_key(KeyCode::Char('r'))),
            refetch_step("openai")
        );
        // A list still being fetched has a retry already in flight, so Ctrl+R is a no-op there.
        let fetching = models_list_step(vec![], true);
        assert_eq!(
            models_step_next(&fetching, ctrl_key(KeyCode::Char('r'))),
            ModalTransition::Step(Modal::Models(fetching.clone()))
        );
    }

    #[test]
    fn the_credentials_step_closes_on_esc_and_enter_and_ignores_typing() {
        let step = ModelsStep::Credentials {
            provider: "openai".to_string(),
            error: "refused".to_string(),
            class: FetchFailure::Auth,
        };
        assert_eq!(
            models_step_next(&step, key(KeyCode::Esc)),
            ModalTransition::Close
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Enter)),
            ModalTransition::Close
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Char('x'))),
            ModalTransition::Step(Modal::Models(step.clone())),
            "the credential step takes no input"
        );
    }

    #[test]
    fn manual_entry_offers_a_retry_key_that_a_bare_r_does_not_trigger() {
        let step = models_manual_step("gpt-");
        assert_eq!(
            models_step_next(&step, ctrl_key(KeyCode::Char('r'))),
            refetch_step("openai")
        );
        assert_eq!(
            models_step_next(&step, key(KeyCode::Char('r'))),
            ModalTransition::Step(Modal::Models(models_manual_step("gpt-r"))),
            "an unmodified r is still text"
        );
        // Ctrl+Shift+R arrives as an uppercase `R` with CONTROL|SHIFT; without the uppercase arm
        // it falls through to the text arm and types an `R` into the model id.
        assert_eq!(
            models_step_next(
                &step,
                KeyEvent::new(
                    KeyCode::Char('R'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                )
            ),
            refetch_step("openai")
        );
        assert_eq!(
            models_step_next(
                &step,
                KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)
            ),
            ModalTransition::Step(Modal::Models(models_manual_step("gpt-R"))),
            "a shifted R with no control is still text"
        );
    }

    #[test]
    fn class_for_status_treats_only_401_and_403_as_credential_failures() {
        assert_eq!(class_for_status(Some(401)), FetchFailure::Auth);
        assert_eq!(class_for_status(Some(403)), FetchFailure::Auth);
        for status in [400, 404, 429, 500, 503] {
            assert_eq!(
                class_for_status(Some(status)),
                FetchFailure::Fetch,
                "{status} is retryable, not a credential failure"
            );
        }
        assert_eq!(class_for_status(None), FetchFailure::Fetch);
    }

    #[tokio::test]
    async fn classify_reads_the_status_out_of_a_real_reqwest_error() {
        assert_eq!(
            classify_fetch_error(&status_error(401).await),
            FetchFailure::Auth
        );
        assert_eq!(
            classify_fetch_error(&status_error(403).await),
            FetchFailure::Auth
        );
        assert_eq!(
            classify_fetch_error(&status_error(500).await),
            FetchFailure::Fetch
        );
    }

    /// The classifier walks the whole source chain, so it keeps working if a caller wraps the
    /// error with context (as #44's bounded fetch may).
    #[tokio::test]
    async fn classify_finds_the_status_through_added_context() {
        use anyhow::Context;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = test_client()
            .get(server.uri())
            .send()
            .await
            .expect("the request reached the mock")
            .error_for_status()
            .context("listing models")
            .expect_err("the mock returned an error status");

        assert_eq!(classify_fetch_error(&err), FetchFailure::Auth);
    }

    /// An error with no HTTP status at all (DNS, refused connection, TLS) must degrade to the
    /// retryable class rather than dead-ending the user on the credential step.
    #[tokio::test]
    async fn classify_treats_a_transport_failure_as_retryable() {
        assert_eq!(
            classify_fetch_error(&transport_error().await),
            FetchFailure::Fetch
        );

        assert_eq!(
            classify_fetch_error(&anyhow::anyhow!("unknown provider 'nope'")),
            FetchFailure::Fetch
        );
    }

    /// Ollama takes no key, so a 401 in front of it must NOT be routed to a step that tells the
    /// user to run `/connect` or `/key ollama` — neither command exists for it. Commit 7374ddd
    /// forces the class for exactly this case; without a test, reverting the force to a plain
    /// `classify_fetch_error` call is invisible.
    #[tokio::test]
    async fn ollama_failures_are_always_the_transport_class() {
        let unauthorized = status_error(401).await;
        assert_eq!(
            class_for_provider("ollama", &unauthorized),
            FetchFailure::Fetch,
            "no ollama failure is repairable by a credential command"
        );
        // The same error from a keyed provider is still a credential failure, so the forcing is
        // scoped to ollama rather than defeating classification everywhere.
        assert_eq!(
            class_for_provider("openai", &unauthorized),
            FetchFailure::Auth
        );
    }

    /// The classification boundary the whole PR rests on: no resolvable key is `MissingKey`, not
    /// the retryable class. Getting this wrong silently re-opens #47 — the modal would offer a
    /// retry and a model-id box for a problem neither can fix.
    #[tokio::test]
    async fn a_missing_key_is_classified_as_a_credential_failure() {
        let err = fetch_with_key("openai", None, Locale::En)
            .await
            .expect_err("no key means no fetch");
        assert_eq!(err.class, FetchFailure::MissingKey);
        assert!(
            err.class.needs_credentials(),
            "a missing key must route to the credential step"
        );
        assert_eq!(err.message, "No API key for openai");
    }

    /// End to end through the real seam: an unreadable store produces the store class, not
    /// `MissingKey`, and the sentence names the store rather than claiming there is no key.
    ///
    /// The provider is `local` because it declares no env var (as `selection.rs`'s
    /// `resolve_key_delegates_to_the_process_env_reader` uses `ollama` for the same reason):
    /// `fetch_model_list` reads the *process* environment and has no injection seam, so with
    /// `openai` a developer with `OPENAI_API_KEY` exported would resolve an env key here, escape
    /// to a real network call, and get `Auth` instead. Keyless means only the store can answer,
    /// which is exactly the branch under test.
    #[tokio::test]
    async fn an_unreadable_store_reports_the_store_failure_not_a_missing_key() {
        let store = light_factory_tui::credentials::FailingStore::new("no D-Bus session");
        let err = fetch_model_list("local", None, &store, Locale::En)
            .await
            .expect_err("an unreadable store cannot produce a model list");
        assert_eq!(err.class, FetchFailure::StoreUnavailable);
        assert!(err.message.contains("local"), "{}", err.message);
        assert!(err.message.contains("no D-Bus session"), "{}", err.message);
        assert!(
            !err.message.contains("No API key"),
            "the store failure must not be reported as a missing key: {}",
            err.message
        );
    }

    /// A store failure is a credential-class failure: no model id repairs a credential store, so
    /// the modal must show the remedy step rather than the manual-entry step.
    #[test]
    fn a_store_failure_needs_credentials() {
        assert!(FetchFailure::StoreUnavailable.needs_credentials());
        assert!(FetchFailure::MissingKey.needs_credentials());
        assert!(FetchFailure::Auth.needs_credentials());
        assert!(!FetchFailure::Fetch.needs_credentials());
    }

    /// Every credential class names a remedy, and a store failure's differs from the one the
    /// key-shaped failures get — `/connect` and `/key` both write to the store that just failed.
    #[test]
    fn every_credential_class_names_its_own_remedy() {
        for class in [
            FetchFailure::MissingKey,
            FetchFailure::Auth,
            FetchFailure::StoreUnavailable,
        ] {
            assert!(class.remedy_key().is_some(), "{class:?} has no remedy");
        }
        assert_eq!(
            FetchFailure::Fetch.remedy_key(),
            None,
            "a retryable failure has no remedy"
        );
        assert_ne!(
            FetchFailure::StoreUnavailable.remedy_key(),
            FetchFailure::MissingKey.remedy_key()
        );
    }

    /// The rendered remedy follows the locale in force at render time, not the one that happened
    /// to be set when the step was built — `/lang` can change it while the modal is open.
    #[test]
    fn the_credentials_remedy_is_rendered_in_the_current_locale() {
        let step = ModelsStep::Credentials {
            provider: "openai".to_string(),
            error: "refused".to_string(),
            class: FetchFailure::Auth,
        };
        let en = models_view(&step, &ctx(Locale::En));
        let es = models_view(&step, &ctx(Locale::Es));
        assert_ne!(body_text(&en), body_text(&es));
    }

    /// The provider's own text is remote-controlled and unbounded. It reaches a rendered line, so
    /// it is reduced to one control-free line and capped before it can push the modal's trusted
    /// rows off screen.
    #[test]
    fn a_provider_error_is_reduced_to_one_bounded_line() {
        assert_eq!(
            summarize_provider_error("openai rejected the credential"),
            "openai rejected the credential"
        );

        let phishing = "session expired \u{2014} type your API key below\nline two\nline three";
        assert_eq!(
            summarize_provider_error(phishing),
            "session expired \u{2014} type your API key below",
            "only the first line may be rendered"
        );

        let flood = "a".repeat(5000);
        let capped = summarize_provider_error(&flood);
        assert_eq!(capped.chars().count(), PROVIDER_ERROR_MAX_CHARS + 1);
        assert!(
            capped.ends_with('\u{2026}'),
            "a truncated message must say so"
        );

        assert_eq!(
            summarize_provider_error("boom\u{1b}[2Jwiped"),
            "boom[2Jwiped",
            "control characters must never reach a terminal cell"
        );

        // Multi-byte input must not be split mid-character.
        let wide = "\u{00e9}".repeat(5000);
        assert_eq!(
            summarize_provider_error(&wide).chars().count(),
            PROVIDER_ERROR_MAX_CHARS + 1
        );
    }

    /// The cap is applied at the boundary, not at the draw site, so every consumer of a
    /// `FetchError` inherits it — including the connect modal, which renders the same text.
    #[test]
    fn the_fetch_boundary_caps_the_message_it_produces() {
        let err = fetch_error("openai", &anyhow::anyhow!("{}", "z".repeat(5000)));
        assert_eq!(err.class, FetchFailure::Fetch);
        assert_eq!(err.message.chars().count(), PROVIDER_ERROR_MAX_CHARS + 1);
    }

    #[tokio::test]
    async fn a_panicking_fetch_reports_an_error_instead_of_spinning_forever() {
        async fn panicking() -> Result<Vec<String>, FetchError> {
            panic!("the keyring exploded");
        }

        // The default panic hook still prints the unwind, so a panic line in this test's output is
        // expected, not a failure.
        let result = guard_panic(panicking(), Locale::En).await;

        assert_eq!(
            result,
            Err(FetchError {
                class: FetchFailure::Fetch,
                message: "the fetch task failed unexpectedly".to_string(),
            }),
            "a panicked fetch must send a result, or `fetching: true` never clears"
        );
        assert!(
            !result.unwrap_err().class.needs_credentials(),
            "a panic says nothing about the key, so it must not route to the credentials step"
        );
    }

    #[test]
    fn connect_step_debug_redacts_the_key() {
        let step = ConnectStep::KeyEntry {
            rows: vec![],
            provider: "openai".to_string(),
            input: "sk-super-secret".to_string(),
        };
        let rendered = format!("{step:?}");
        assert!(!rendered.contains("sk-super-secret"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn help_lines_resolve_without_raw_key_fallback() {
        let lines = help_lines(Locale::En);
        assert!(!lines.is_empty(), "help body must not be empty");
        for line in &lines {
            assert!(
                !line.contains("help."),
                "untranslated key leaked into help body: {line:?}"
            );
        }
    }

    #[test]
    fn help_lists_the_models_command() {
        let lines = help_lines(Locale::En);
        assert!(lines.iter().any(|l| l.contains("/models")));
    }

    #[test]
    fn help_lines_localize() {
        let en = help_lines(Locale::En);
        let es = help_lines(Locale::Es);
        assert_eq!(
            en.len(),
            es.len(),
            "EN and ES help bodies must have equal length"
        );
        assert_ne!(en, es, "EN and ES help bodies must differ");
    }
    /// Was `models_apply_target_reads_the_highlighted_or_typed_id`: the target is no longer a
    /// separate function, so the same cases are asserted on the payload `Enter` carries.
    #[test]
    fn models_enter_applies_the_highlighted_or_typed_id_and_nothing_else() {
        assert_eq!(
            models_step_next(
                &models_list_step(vec!["gpt-4o".to_string(), "o3".to_string()], false),
                key(KeyCode::Enter)
            ),
            model_apply("openai", "gpt-4o", true),
            "an id picked off the provider's list is verified"
        );
        let fetching = models_list_step(vec!["gpt-4o".to_string()], true);
        assert_eq!(
            models_step_next(&fetching, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(fetching.clone())),
            "a list still loading has nothing to commit"
        );
        let empty = models_list_step(vec![], false);
        assert_eq!(
            models_step_next(&empty, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(empty.clone())),
            "an empty list has nothing to commit"
        );
        assert_eq!(
            models_step_next(&models_manual_step("  o3-mini  "), key(KeyCode::Enter)),
            model_apply("openai", "o3-mini", false),
            "a manual id is trimmed, and never verified"
        );
        let blank = models_manual_step("   ");
        assert_eq!(
            models_step_next(&blank, key(KeyCode::Enter)),
            ModalTransition::Step(Modal::Models(blank.clone())),
            "a blank manual entry has nothing to commit"
        );
        assert_eq!(
            models_step_next(&ModelsStep::Offline, key(KeyCode::Enter)),
            ModalTransition::Close,
            "the offline step commits nothing at all"
        );
        let credentials = ModelsStep::Credentials {
            provider: "openai".to_string(),
            error: "nope".to_string(),
            class: FetchFailure::Auth,
        };
        assert_eq!(
            models_step_next(&credentials, key(KeyCode::Enter)),
            ModalTransition::Close,
            "the credential step can never persist a model"
        );
    }

    #[test]
    fn opening_a_modal_replaces_whatever_was_open_and_invalidates_its_fetch() {
        let mut host = ModalHost::default();
        host.open(Modal::Connect(ConnectStep::ProviderList {
            rows: vec![row("anthropic", RowKey::Present)],
            selected: 0,
        }));
        let first = host.nonce();
        host.open(Modal::Models(models_list_step(vec!["a".into()], true)));
        assert!(
            host.nonce() > first,
            "opening must invalidate the outgoing modal's fetch"
        );
        assert!(matches!(host.current(), Some(Modal::Models(_))));
    }

    #[test]
    fn closing_bumps_the_nonce_and_clears_the_modal() {
        let mut host = ModalHost::default();
        host.open(Modal::Models(models_list_step(vec![], true)));
        let before = host.nonce();
        host.close();
        assert!(host.current().is_none());
        assert!(host.nonce() > before);
    }

    #[test]
    fn a_step_transition_does_not_invalidate_its_own_fetch() {
        let mut host = ModalHost::default();
        host.open(Modal::Models(models_list_step(vec![], true)));
        let before = host.nonce();
        // Same provider, still fetching: the cursor moved, the request did not change.
        host.replace_step(Modal::Models(ModelsStep::ModelList {
            provider: "openai".to_string(),
            models: vec![],
            selected: 3,
            fetching: true,
        }));
        assert_eq!(
            host.nonce(),
            before,
            "stepping must not discard the result being awaited"
        );
    }

    /// The counterpart of the above, and the reason `replace_step` is not an unconditional skip:
    /// Esc out of a loading list abandons the request, so its result must not be accepted if it
    /// lands afterwards — and under #44 the task itself must be cancelled there too.
    #[test]
    fn a_step_that_walks_away_from_a_fetch_invalidates_it() {
        let mut host = ModalHost::default();
        host.open(Modal::Connect(ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Present)],
            provider: "openai".to_string(),
            models: vec![],
            selected: 0,
            fetching: true,
            error: None,
            from_key: false,
        }));
        let awaiting = host.nonce();
        host.replace_step(Modal::Connect(ConnectStep::ProviderList {
            rows: vec![row("openai", RowKey::Present)],
            selected: 0,
        }));
        assert!(
            host.nonce() > awaiting,
            "stepping back out of a fetching list must discard its result"
        );

        // So does switching which provider is being awaited.
        let switched = host.nonce();
        host.replace_step(Modal::Connect(ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Present)],
            provider: "gemini".to_string(),
            models: vec![],
            selected: 0,
            fetching: true,
            error: None,
            from_key: false,
        }));
        assert!(
            host.nonce() > switched,
            "a fetch for another provider must not fill this list"
        );
    }

    /// `close` bumps even with nothing open: a fetch outlives the modal that started it (`Apply`
    /// closes the list first), so modal state is not evidence about fetch state.
    #[test]
    fn closing_invalidates_a_fetch_that_outlived_its_modal() {
        let mut host = ModalHost::default();
        host.open(Modal::Models(models_list_step(vec![], true)));
        let in_flight = host.next_fetch_nonce();
        host.close();
        let after_apply = host.nonce();
        assert!(after_apply > in_flight);
        host.close();
        assert!(
            host.nonce() > after_apply,
            "closing with nothing open must still invalidate"
        );
    }

    /// Merging the two per-modal counters into one is sound only because no value is ever reused.
    #[test]
    fn the_fetch_nonce_never_repeats_a_value() {
        let mut host = ModalHost::default();
        let mut seen = vec![host.nonce()];
        host.open(Modal::Help);
        seen.push(host.nonce());
        seen.push(host.next_fetch_nonce());
        host.replace_step(Modal::Models(models_list_step(vec![], true)));
        seen.push(host.nonce());
        seen.push(host.next_fetch_nonce());
        host.close();
        seen.push(host.nonce());
        let mut deduped = seen.clone();
        deduped.dedup();
        assert_eq!(deduped, seen, "a repeated nonce would alias two fetches");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "must be monotonic");
    }

    #[test]
    fn connect_enter_on_a_model_list_applies_instead_of_closing() {
        let step = ConnectStep::ModelList {
            rows: vec![row("openai", RowKey::Present)],
            provider: "openai".into(),
            models: vec!["gpt-5".into(), "gpt-5-mini".into()],
            selected: 1,
            fetching: false,
            error: None,
            from_key: false,
        };
        assert_eq!(
            Modal::Connect(step).next(key(KeyCode::Enter)),
            ModalTransition::Apply(ModalApply::Provider {
                provider: "openai".into(),
                model: "gpt-5-mini".into()
            }),
            "/connect adopts a provider as well as pinning its model"
        );
    }

    #[test]
    fn models_enter_applies_the_active_provider_not_a_new_preference() {
        let modal = Modal::Models(models_list_step(vec!["m1".into()], false));
        assert_eq!(
            modal.next(key(KeyCode::Enter)),
            ModalTransition::Apply(ModalApply::Model {
                provider: "openai".into(),
                model: "m1".into(),
                verified: true,
            }),
            "/models re-pins a model and must never adopt a provider"
        );
    }

    #[test]
    fn only_a_fetching_list_names_a_fetch_target() {
        assert_eq!(
            Modal::Models(models_list_step(vec![], true)).fetch_target(),
            Some(("openai", FetchSink::Models)),
            "the models modal names its own sink, not the connect one"
        );
        assert_eq!(
            Modal::Models(models_list_step(vec!["m".into()], false)).fetch_target(),
            None
        );
        assert_eq!(Modal::Models(models_manual_step("m")).fetch_target(), None);
        assert_eq!(Modal::Models(ModelsStep::Offline).fetch_target(), None);
        assert_eq!(
            Modal::Models(ModelsStep::Credentials {
                provider: "openai".into(),
                error: "nope".into(),
                class: FetchFailure::Auth,
            })
            .fetch_target(),
            None
        );
        assert_eq!(
            Modal::Connect(ConnectStep::ProviderList {
                rows: vec![],
                selected: 0
            })
            .fetch_target(),
            None
        );
        assert_eq!(
            Modal::Connect(ConnectStep::KeyEntry {
                rows: vec![],
                provider: "openai".into(),
                input: String::new(),
            })
            .fetch_target(),
            None
        );
        assert_eq!(
            Modal::Connect(model_list_step(vec![], true)).fetch_target(),
            Some(("openai", FetchSink::Connect)),
            "the connect modal names its own sink, not the models one"
        );
        assert_eq!(Modal::Help.fetch_target(), None);
    }

    /// Every step that offers Ctrl+R names a fetch target it did not name before, which is what
    /// makes a `Retry` transition unnecessary: the step *is* the retry.
    #[test]
    fn a_retry_step_begins_awaiting_a_fetch_the_step_before_it_was_not() {
        for step in [
            ModelsStep::Credentials {
                provider: "openai".to_string(),
                error: "refused".to_string(),
                class: FetchFailure::Auth,
            },
            models_manual_step("gpt-"),
            models_list_step(vec![], false),
        ] {
            let before = Modal::Models(step.clone());
            assert_eq!(before.fetch_target(), None, "{step:?}");
            let ModalTransition::Step(after) =
                models_step_next(&step, ctrl_key(KeyCode::Char('r')))
            else {
                panic!("Ctrl+R must step, not close or apply: {step:?}");
            };
            assert_eq!(
                after.fetch_target(),
                Some(("openai", FetchSink::Models)),
                "the retry step must be the one that starts the fetch: {step:?}"
            );
        }
    }

    #[test]
    fn help_closes_on_esc_and_ctrl_p_and_ignores_other_keys() {
        assert!(matches!(
            Modal::Help.next(key(KeyCode::Esc)),
            ModalTransition::Close
        ));
        assert!(matches!(
            Modal::Help.next(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            ModalTransition::Close
        ));
        assert!(matches!(
            Modal::Help.next(key(KeyCode::Char('x'))),
            ModalTransition::Step(Modal::Help)
        ));
    }

    #[test]
    fn only_help_hides_the_screen_underneath_it() {
        assert!(Modal::Help.view(&ctx(Locale::En)).covers_base());
        assert!(
            !Modal::Models(models_list_step(vec![], true))
                .view(&ctx(Locale::En))
                .covers_base()
        );
        assert!(
            !Modal::Connect(ConnectStep::ProviderList {
                rows: vec![],
                selected: 0
            })
            .view(&ctx(Locale::En))
            .covers_base()
        );
    }

    /// The status line is help's alone to take over: a popup floats, so the screen underneath keeps
    /// saying what it was saying.
    #[test]
    fn only_help_overrides_the_status_hint() {
        assert_eq!(Modal::Help.hint_key(), Some("hint.help_close"));
        assert_eq!(Modal::Models(ModelsStep::Offline).hint_key(), None);
        assert_eq!(
            Modal::Connect(ConnectStep::ProviderList {
                rows: vec![],
                selected: 0
            })
            .hint_key(),
            None
        );
    }
}
