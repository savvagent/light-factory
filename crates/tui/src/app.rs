//! The ratatui application: auth forms plus the connected WebSocket screen.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use light_factory_engine::Engine;
use light_factory_protocol::auth::AuthResponse;
use light_factory_protocol::session::{Command, Event as EngineEvent, EventKind, SessionId};
use light_factory_protocol::wire::{ClientMessage, ServerMessage};
use light_factory_providers::{CompleteRequest, Provider};
use light_factory_tui::credentials::CredentialStore;
use light_factory_tui::engine_view::{describe_event, pending_prompt};
use light_factory_tui::i18n::{self, Locale};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tokio::sync::mpsc;

use crate::api::{Api, ApiError};
use crate::browser;
use crate::config::Config;
#[cfg(test)]
use crate::modal::fetch_error;
use crate::modal::{
    ConnectStep, FetchError, FetchFailure, FetchSink, Modal, ModalApply, ModalContext, ModalHost,
    ModalTransition, ModelsStep, ProviderRow, fetch_model_list, mask,
};
use crate::provider::ProviderInfo;
use crate::selection::takes_key;
use crate::session::Session;
use crate::settings::{Settings, SettingsHandle};
use crate::ws;

/// Events flowing into the single UI loop.
pub enum UiEvent {
    Key(KeyEvent),
    Server(ServerMessage),
    Device {
        nonce: u64,
        result: Result<AuthResponse, ApiError>,
    },
    Completion(Result<String, String>),
    Engine(EngineEvent),
    EngineDropped(u64),
    ConnectModels {
        nonce: u64,
        provider: String,
        result: Result<Vec<String>, String>,
    },
    ModelsFetched {
        nonce: u64,
        provider: String,
        result: Result<Vec<String>, FetchError>,
    },
    /// The verification probe for a key `/key` has just stored. Carries no model list: `/key` has
    /// no renderer for one, and not carrying it keeps a provider-supplied payload off this path.
    KeyProbed {
        nonce: u64,
        provider: String,
        result: Result<(), FetchError>,
    },
}

/// Which field currently owns keyboard input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Email,
    Name,
    Code,
}

/// The screen currently shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    SignIn,
    Register,
    RegisterCode,
    Device,
    Connected,
    Engine,
    Key,
}

const LOG_CAPACITY: usize = 200;
const KEEPALIVE_SECONDS: u64 = 30;

pub struct App {
    config: Config,
    api: Api,
    events: mpsc::UnboundedSender<UiEvent>,
    mode: Mode,
    focus: Focus,
    email: String,
    name: String,
    code: String,
    command: String,
    command_mode: bool,
    device_nonce: u64,
    device_user_code: Option<String>,
    device_verification_uri: Option<String>,
    setup_token: Option<String>,
    secret: Option<String>,
    otpauth_url: Option<String>,
    session: Option<Session>,
    ws_tx: Option<mpsc::UnboundedSender<ClientMessage>>,
    provider: Arc<dyn Provider>,
    provider_info: ProviderInfo,
    store: Arc<dyn CredentialStore>,
    settings: Settings,
    settings_path: PathBuf,
    key_target: Option<String>,
    key_input: String,
    key_return: Mode,
    /// Generation of the most recent `/key` verification probe. A second `/key` submitted while the
    /// first probe is still in flight bumps this, so the older result — which answers a question
    /// the user has already replaced — fails the check in [`App::handle_key_probed`] and is
    /// discarded rather than overwriting a newer status. Starts at 0 and is incremented *before*
    /// use, so no live probe ever carries nonce 0.
    key_probe_nonce: u64,
    /// The in-flight `/key` verification probe, if any. Held so [`App::next_key_probe_nonce`] can
    /// abort it: the request carries an API key in its headers, and two probes in flight is two
    /// keys in flight. Deliberately not inside [`ModalHost`] — `/key` is `Mode::Key`, a screen, and
    /// the modal host's nonce exists to discard results that outlive their *modal*.
    key_probe: Option<tokio::task::JoinHandle<()>>,
    /// The one overlay that owns the keyboard, if any, together with its fetch generation and
    /// cancellation handle. One field, not six: "two modals open at once" is unrepresentable.
    modal: ModalHost,
    engine: Option<Engine>,
    engine_session: Option<SessionId>,
    engine_forward_task: Option<tokio::task::JoinHandle<()>>,
    engine_log: Vec<String>,
    engine_prompt: String,
    pending: Option<(EventKind, String)>,
    error: Option<String>,
    status: String,
    log: VecDeque<String>,
    nonce: u64,
    pongs: u64,
}

impl App {
    fn new(
        config: Config,
        provider: Arc<dyn Provider>,
        provider_info: ProviderInfo,
        store: Arc<dyn CredentialStore>,
        settings: SettingsHandle,
        prefilled_email: Option<String>,
        events: mpsc::UnboundedSender<UiEvent>,
    ) -> Self {
        let api = Api::new(&config.http_base);
        let status = i18n::t(config.lang, "status.not_signed_in").to_string();
        Self {
            config,
            api,
            events,
            mode: Mode::SignIn,
            focus: Focus::Email,
            email: prefilled_email.unwrap_or_default(),
            name: String::new(),
            code: String::new(),
            command: String::new(),
            command_mode: false,
            device_nonce: 0,
            device_user_code: None,
            device_verification_uri: None,
            setup_token: None,
            secret: None,
            otpauth_url: None,
            session: None,
            ws_tx: None,
            provider,
            provider_info,
            store,
            settings: settings.settings,
            settings_path: settings.path,
            key_target: None,
            key_probe_nonce: 0,
            key_probe: None,
            key_input: String::new(),
            key_return: Mode::SignIn,
            modal: ModalHost::default(),
            engine: None,
            engine_session: None,
            engine_forward_task: None,
            engine_log: Vec::new(),
            engine_prompt: String::new(),
            pending: None,
            error: None,
            status,
            log: VecDeque::new(),
            nonce: 0,
            pongs: 0,
        }
    }

    fn t<'a>(&self, key: &'a str) -> &'a str {
        i18n::t(self.config.lang, key)
    }

    fn t_with(&self, key: &str, params: &[(&str, &str)]) -> String {
        i18n::t_with(self.config.lang, key, params)
    }

    fn error_text(&self, code: &str, message: &str) -> String {
        i18n::error_message(self.config.lang, code)
            .map(str::to_string)
            .unwrap_or_else(|| message.to_string())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.modal.is_open() {
            return self.handle_modal_key(key);
        }
        if key.code == KeyCode::Char('p')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !self.command_mode
        {
            self.open_help();
            return false;
        }
        if self.command_mode {
            return self.handle_command_key(key).await;
        }
        if self.mode == Mode::Engine {
            return self.handle_engine_key(key).await;
        }
        if self.mode == Mode::Key {
            return self.handle_key_entry(key);
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc => match self.mode {
                Mode::SignIn => return true,
                Mode::Register => {
                    self.mode = Mode::SignIn;
                    self.code.clear();
                    self.error = None;
                }
                Mode::RegisterCode => {
                    self.mode = Mode::Register;
                    self.code.clear();
                    self.error = None;
                }
                Mode::Device => {
                    self.device_nonce += 1;
                    self.device_user_code = None;
                    self.device_verification_uri = None;
                    self.mode = Mode::SignIn;
                    self.status = self.t("status.device_cancelled").to_string();
                    self.error = None;
                }
                Mode::Connected => {}
                Mode::Engine => {}
                Mode::Key => {}
            },
            KeyCode::Char('/')
                if matches!(
                    self.mode,
                    Mode::SignIn | Mode::Register | Mode::RegisterCode | Mode::Connected
                ) =>
            {
                self.command_mode = true;
                self.command = "/".to_string();
                self.error = None;
            }
            KeyCode::Char('q') => {
                if self.mode == Mode::Connected {
                    return true;
                }
                self.type_char('q');
            }
            KeyCode::Char('p') if self.mode == Mode::Connected => self.ping(),
            KeyCode::Char('o') if self.mode == Mode::Connected => self.sign_out().await,
            KeyCode::Char('e') if self.mode == Mode::Connected => {
                if let Err(e) = self.enter_engine() {
                    self.error = Some(e.to_string());
                }
            }
            KeyCode::Char(c) => self.type_char(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => self.cycle_focus(),
            KeyCode::Enter => self.submit().await,
            _ => {}
        }
        false
    }

    async fn handle_engine_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc => self.leave_engine(),
            KeyCode::Enter => self.engine_send_prompt(),
            KeyCode::Backspace => {
                self.engine_prompt.pop();
            }
            KeyCode::Char(c) => match engine_approval_key(c, self.pending.is_some()) {
                Some(approved) => self.engine_answer(approved),
                None => self.engine_prompt.push(c),
            },
            _ => {}
        }
        false
    }

    fn open_help(&mut self) {
        self.open_modal(Modal::Help, None);
    }

    fn enter_engine(&mut self) -> anyhow::Result<()> {
        let provider = self.provider.clone();
        let info = self.provider_info.clone();

        let mut engine = Engine::new(provider);
        let session = engine.create_session(std::env::current_dir()?)?;

        let mut events = engine.handle(session).expect("just created").subscribe();
        let tx = self.events.clone();
        let forwarder = tokio::spawn(async move {
            loop {
                match engine_forward_step(events.recv().await) {
                    EngineForward::Event(event) => {
                        if tx.send(UiEvent::Engine(event)).is_err() {
                            break;
                        }
                    }
                    EngineForward::Dropped(n) => {
                        if tx.send(UiEvent::EngineDropped(n)).is_err() {
                            break;
                        }
                    }
                    EngineForward::Stop => break,
                }
            }
        });

        self.engine = Some(engine);
        self.engine_session = Some(session);
        self.engine_forward_task = Some(forwarder);
        self.engine_log.clear();
        for warning in info.warnings {
            self.engine_log.push(warning);
        }
        if let Some(reason) = &info.offline {
            self.engine_log
                .push(crate::provider::offline_notice(self.config.lang, reason));
        }
        self.engine_prompt.clear();
        self.pending = None;
        self.mode = Mode::Engine;
        self.status = self.t("status.engine_started").to_string();
        Ok(())
    }

    fn leave_engine(&mut self) {
        if let Some(forwarder) = self.engine_forward_task.take() {
            forwarder.abort();
        }
        self.engine = None;
        self.engine_session = None;
        self.mode = Mode::Connected;
        self.engine_prompt.clear();
        self.pending = None;
        if let Some(s) = &self.session {
            self.status = self.t_with("status.connected_as", &[("email", &s.email)]);
        }
    }

    fn engine_answer(&mut self, approved: bool) {
        let (Some(engine), Some(session)) = (self.engine.as_mut(), self.engine_session) else {
            return;
        };
        let command = match self.pending.as_ref().map(|(kind, _)| kind) {
            Some(EventKind::PlanProposed { plan_id, .. }) => Some(Command::ApprovePlan {
                session,
                plan_id: *plan_id,
                approved,
            }),
            Some(EventKind::ApprovalRequest { request_id, .. }) => Some(Command::ApproveAction {
                session,
                request_id: *request_id,
                approved,
            }),
            _ => None,
        };
        if let Some(command) = command {
            let _ = engine.dispatch(command);
            self.pending = None;
        }
    }

    fn engine_send_prompt(&mut self) {
        let text = self.engine_prompt.trim().to_string();
        if text.is_empty() {
            return;
        }
        let (Some(engine), Some(session)) = (self.engine.as_mut(), self.engine_session) else {
            return;
        };
        let _ = engine.dispatch(Command::SendPrompt {
            session,
            text: text.clone(),
        });
        self.engine_log.push(format!("> {text}"));
        self.engine_prompt.clear();
    }

    fn handle_engine_event(&mut self, event: EngineEvent) {
        let line = describe_event(self.config.lang, &event.kind);
        self.engine_log.push(line);
        while self.engine_log.len() > LOG_CAPACITY {
            self.engine_log.remove(0);
        }
        if let Some(prompt) = pending_prompt(self.config.lang, &event.kind) {
            self.pending = Some((event.kind, prompt));
        }
    }

    fn handle_engine_dropped(&mut self, n: u64) {
        let count = n.to_string();
        let line = i18n::t_with(
            self.config.lang,
            "engine.dropped_events",
            &[("count", &count)],
        );
        self.engine_log.push(line);
        while self.engine_log.len() > LOG_CAPACITY {
            self.engine_log.remove(0);
        }
    }

    async fn handle_command_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc => {
                self.command_mode = false;
                self.command.clear();
            }
            KeyCode::Enter => {
                let command = self.command.clone();
                self.command_mode = false;
                self.command.clear();
                self.run_command(&command).await;
            }
            KeyCode::Backspace => {
                self.command.pop();
            }
            KeyCode::Char(c) => self.command.push(c),
            _ => {}
        }
        false
    }

    fn handle_key_entry(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc => self.cancel_key_entry(),
            KeyCode::Enter => self.submit_key_entry(),
            KeyCode::Backspace => {
                self.key_input.pop();
            }
            KeyCode::Char(c) => self.key_input.push(c),
            _ => {}
        }
        false
    }

    fn cancel_key_entry(&mut self) {
        self.key_target = None;
        self.key_input.clear();
        self.mode = self.key_return;
    }

    fn submit_key_entry(&mut self) {
        let Some(provider) = self.key_target.clone() else {
            self.mode = self.key_return;
            return;
        };
        let key = self.key_input.trim().to_string();
        self.key_target = None;
        self.key_input.clear();
        self.mode = self.key_return;
        if key.is_empty() {
            self.status = self.t("status.key_empty").to_string();
            return;
        }
        match self.store.set(&provider, &key) {
            Ok(()) => {
                self.rebuild_provider();
                // The floor, set unconditionally and before the probe exists: if nothing ever
                // comes back — the process dies, the next `/key` aborts this one, the fetch hangs
                // to its deadline — "not yet verified" is still exactly true. The probe below only
                // ever replaces this with a sharper sentence, so the honesty of the status never
                // depends on a network round-trip landing (#61).
                self.status =
                    self.t_with("status.key_stored_unverified", &[("provider", &provider)]);
                self.begin_key_probe(provider, key);
            }
            Err(e) => {
                let error = e.to_string();
                self.error = Some(self.t_with(
                    "status.key_failed",
                    &[("provider", &provider), ("error", &error)],
                ));
            }
        }
    }

    /// Claim the next `/key` probe generation, cancelling and invalidating whatever probe was in
    /// flight.
    ///
    /// Bumping and aborting are one operation for the reason [`ModalHost::next_fetch_nonce`] gives:
    /// they are the same event, so no call site can perform one and forget the other — and
    /// forgetting the abort would leave a request carrying the previous API key in its headers
    /// running against an answer nobody will read.
    ///
    /// `wrapping_add` because a `u64` count of `/key` submissions cannot realistically wrap, and a
    /// panic there would be a worse outcome than a reused generation.
    fn next_key_probe_nonce(&mut self) -> u64 {
        self.key_probe_nonce = self.key_probe_nonce.wrapping_add(1);
        if let Some(previous) = self.key_probe.take() {
            previous.abort();
        }
        self.key_probe_nonce
    }

    /// Ask the provider whether the key just stored is one it accepts, off the UI loop.
    ///
    /// The probe is a model-list fetch because that is what the codebase has, what `/connect`
    /// already runs after its own key entry, and what #55 already classifies into
    /// [`FetchFailure`] — so `/key` and `/connect` now agree about whether they checked.
    ///
    /// `Some(key)` is load-bearing, never `None`: `None` would let `fetch_model_list` call
    /// `resolve_key`, which prefers an exported `OPENAI_API_KEY` over the keyring, and the user
    /// would be told the key they just typed was accepted on the strength of a different
    /// credential entirely.
    ///
    /// The model list is discarded at the boundary — `/key` has no use for it.
    fn begin_key_probe(&mut self, provider: String, key: String) {
        let nonce = self.next_key_probe_nonce();
        let events = self.events.clone();
        let store = self.store.clone();
        let lang = self.config.lang;
        self.key_probe = Some(tokio::spawn(async move {
            let result = fetch_model_list(&provider, Some(key), store.as_ref(), lang)
                .await
                .map(|_| ());
            let _ = events.send(UiEvent::KeyProbed {
                nonce,
                provider,
                result,
            });
        }));
    }

    /// Replace the stored-unverified status with what the probe actually learned.
    ///
    /// The key is never un-stored, on any outcome: a 401 can come from an auth-edge outage, an IP
    /// allowlist, or an org-level block as easily as from a bad credential, and `/key <provider>
    /// clear` is the user's to run. The status carries the news; the store carries their intent.
    ///
    /// [`FetchError::message`] is deliberately not interpolated. It is provider-supplied text on a
    /// path whose whole subject is a live credential, so every `/key` status takes `{provider}` and
    /// nothing else.
    fn handle_key_probed(&mut self, nonce: u64, provider: String, result: Result<(), FetchError>) {
        if nonce != self.key_probe_nonce {
            return;
        }
        // The nonce matched, so this is the current probe's own result and that task is done.
        self.key_probe = None;
        let status = match &result {
            Ok(()) => "status.key_verified",
            // The one predicate #55 draws: a credential failure, versus anything that says nothing
            // about the key.
            Err(e) if e.class.needs_credentials() => "status.key_rejected",
            Err(_) => "status.key_unreachable",
        };
        self.status = self.t_with(status, &[("provider", &provider)]);
    }

    /// Tear down any open modal, and cancel and invalidate its in-flight fetch.
    ///
    /// Called from every path that replaces the screen underneath a modal without the user asking:
    /// losing the session, and the device-login result landing asynchronously under an overlay the
    /// user opened while waiting for it. A modal is deliberately not a `Mode`, so assigning
    /// `self.mode` no longer tears one down implicitly — without this call a help overlay opened
    /// during device login survives into the screen that replaced it, suppressing that screen
    /// entirely and swallowing every key but Ctrl-C until Esc.
    ///
    /// `ModalHost::close` is unconditional for the same reason its predecessor was: cancellation is
    /// tied to task state, not modal state, so no state combination can strand a live fetch.
    fn dismiss_modals(&mut self) {
        self.modal.close();
    }

    /// Persist the settings, surfacing a failure as a user-visible error rather than swallowing
    /// it. Returns whether the write landed, so callers can roll back the change they staged.
    fn persist_settings(&mut self) -> bool {
        match crate::settings::save_at(&self.settings_path, &self.settings) {
            Ok(()) => true,
            Err(e) => {
                // `{:#}` keeps anyhow's source chain; `to_string` would drop the actual cause.
                let err = format!("{e:#}");
                self.error = Some(self.t_with("status.settings_save_failed", &[("error", &err)]));
                false
            }
        }
    }

    /// Stage a model for a provider and persist it, rolling the in-memory map back if the write
    /// fails so a later unrelated save cannot silently resurrect it.
    ///
    /// `verified` says whether the id came off the provider's own model list. A blindly typed id
    /// reports a distinct status, so "Model set to o3" never implies the provider confirmed it.
    fn persist_model(&mut self, provider: String, model: String, verified: bool) -> bool {
        let previous = self.settings.models.insert(provider.clone(), model.clone());
        if self.persist_settings() {
            self.rebuild_provider();
            self.status = if verified {
                self.t_with("status.model_set", &[("model", &model)])
            } else {
                self.t_with(
                    "status.model_set_unverified",
                    &[("model", &model), ("provider", &provider)],
                )
            };
            return true;
        }
        match previous {
            Some(old) => self.settings.models.insert(provider, old),
            None => self.settings.models.remove(&provider),
        };
        false
    }

    fn rebuild_provider(&mut self) {
        let (provider, info) = crate::selection::rebuild(&self.settings, self.store.as_ref());
        self.provider = provider;
        self.provider_info = info;
    }

    async fn run_command(&mut self, command: &str) {
        self.error = None;
        let trimmed = command.trim();
        if let Some(prompt) = parse_ask_command(trimmed) {
            if self.mode == Mode::Connected {
                self.ask(prompt);
            } else {
                self.error = Some(self.t("status.ask_not_connected").to_string());
            }
            return;
        }
        if trimmed.starts_with("/ask") {
            self.error = Some(self.t("status.ask_empty").to_string());
            return;
        }
        if parse_connect_command(trimmed) {
            if self.mode == Mode::Connected {
                self.enter_connect();
            } else {
                self.error = Some(self.t("status.connect_not_connected").to_string());
            }
            return;
        }
        if parse_models_command(trimmed) {
            if self.mode == Mode::Connected {
                self.enter_models();
            } else {
                self.error = Some(self.t("status.models_not_connected").to_string());
            }
            return;
        }
        if let Some(model) = parse_model_command(trimmed) {
            match model {
                Some(id) => self.set_model(id),
                None => self.error = Some(self.t("status.model_empty").to_string()),
            }
            return;
        }
        if let Some(key_command) = parse_key_command(trimmed) {
            match key_command {
                KeyCommand::List => self.list_keys(),
                KeyCommand::Set(provider) => self.begin_key_entry(provider),
                KeyCommand::Clear(provider) => self.clear_key(&provider),
            }
            return;
        }
        match trimmed {
            "/auth/login" => self.start_device_login().await,
            "/auth/logout" => self.sign_out().await,
            "" => {}
            other if other.starts_with("/lang ") => {
                let arg = other["/lang ".len()..].trim();
                if let Some(locale) = Locale::parse(arg) {
                    let previous_lang = self.config.lang;
                    let previous_saved =
                        std::mem::replace(&mut self.settings.lang, locale.as_str().to_string());
                    self.config.lang = locale;
                    if self.persist_settings() {
                        self.status = self.t_with("status.lang_set", &[("lang", locale.as_str())]);
                    } else {
                        self.config.lang = previous_lang;
                        self.settings.lang = previous_saved;
                    }
                } else {
                    self.error = Some(self.t("status.lang_invalid").to_string());
                }
            }
            other => {
                self.error = Some(self.t_with("status.unknown_command", &[("command", other)]))
            }
        }
    }

    fn enter_connect(&mut self) {
        let rows = self.build_provider_rows();
        self.open_modal(
            Modal::Connect(ConnectStep::ProviderList { rows, selected: 0 }),
            None,
        );
    }

    /// Open `modal`, replacing any other, and start its model-list fetch if it is waiting on one.
    fn open_modal(&mut self, modal: Modal, key: Option<String>) {
        let target = modal
            .fetch_target()
            .map(|(provider, sink)| (provider.to_string(), sink));
        self.modal.open(modal);
        if let Some((provider, sink)) = target {
            self.begin_model_fetch(provider, sink, key);
        }
    }

    fn build_provider_rows(&self) -> Vec<ProviderRow> {
        PROVIDER_NAMES
            .iter()
            .map(|id| {
                let connected = if *id == "ollama" {
                    std::env::var("LIGHT_OLLAMA").as_deref() == Ok("1")
                } else {
                    crate::selection::key_status(id, self.store.as_ref())
                        != crate::selection::KeyStatus::None
                };
                ProviderRow {
                    id: id.to_string(),
                    connected,
                }
            })
            .collect()
    }

    /// Commit what the transition decided, then close the modal.
    ///
    /// `apply` is the value the transition computed from the state that authorised it, not a second
    /// read of `self.modal`: the two commits carry different privilege — `Provider` changes which
    /// service receives the user's prompts and key, `Model` only re-pins a model on the provider
    /// already active — and re-deriving the choice after `close()` would decide it from state that
    /// had already moved.
    ///
    /// Closing first is the `/models` ordering; `persist_model` never touches the modal, so the
    /// order is not observable.
    fn apply_and_close_modal(&mut self, apply: ModalApply) {
        self.modal.close();
        match apply {
            ModalApply::Provider { provider, model } => {
                let previous_provider = self.settings.provider.replace(provider.clone());
                // A model taken from the provider's own list is verified by construction.
                if !self.persist_model(provider, model, true) {
                    self.settings.provider = previous_provider;
                }
            }
            ModalApply::Model {
                provider,
                model,
                verified,
            } => {
                self.persist_model(provider, model, verified);
            }
        }
    }

    /// Fetch a provider's model list off the UI loop for the open modal.
    ///
    /// The nonce claimed here is what makes a result that outlives its modal discardable, and
    /// claiming it also aborts whatever fetch the host was previously awaiting — the two are the
    /// same event, inside [`ModalHost`], so neither can be forgotten at a call site.
    ///
    /// `sink` comes from the same [`Modal::fetch_target`] that named `provider`, so which event
    /// carries the answer back is decided by the state that asked for the fetch rather than by
    /// inspecting `self.modal` once the task is already being spawned.
    fn begin_model_fetch(&mut self, provider: String, sink: FetchSink, key: Option<String>) {
        let nonce = self.modal.next_fetch_nonce();
        let events = self.events.clone();
        let store = self.store.clone();
        let lang = self.config.lang;
        let task = tokio::spawn(async move {
            let result = fetch_model_list(&provider, key, store.as_ref(), lang).await;
            let event = match sink {
                // The connect modal renders only the message; #47's classification is consumed by
                // the `/models` modal alone.
                FetchSink::Connect => UiEvent::ConnectModels {
                    nonce,
                    provider,
                    result: result.map_err(|e| e.message),
                },
                FetchSink::Models => UiEvent::ModelsFetched {
                    nonce,
                    provider,
                    result,
                },
            };
            let _ = events.send(event);
        });
        self.modal.track_fetch(task);
    }

    fn handle_connect_models(
        &mut self,
        nonce: u64,
        provider: String,
        result: Result<Vec<String>, String>,
    ) {
        if nonce != self.modal.nonce() {
            return;
        }
        // The nonce matched, so this is the current fetch's own result and that task is done.
        // Drop its handle rather than leaving a finished task tracked: the handle is a cancellation
        // handle, and a stale `Some` invites a future reader to treat it as "already fetching".
        self.modal.forget_fetch();
        let matches = matches!(
            self.modal.current(),
            Some(Modal::Connect(ConnectStep::ModelList {
                provider: p,
                fetching: true,
                ..
            })) if *p == provider
        );
        if !matches {
            return;
        }
        let err_msg = result
            .as_ref()
            .err()
            .map(|e| self.t_with("connect.fetch_error", &[("error", e)]));
        if let Some(Modal::Connect(ConnectStep::ModelList {
            models,
            selected,
            fetching,
            error,
            ..
        })) = self.modal.current_mut()
        {
            match result {
                Ok(list) => {
                    *models = list;
                    *selected = 0;
                    *fetching = false;
                    *error = None;
                }
                Err(_) => {
                    *fetching = false;
                    *error = err_msg;
                }
            }
        }
    }

    fn enter_models(&mut self) {
        let provider = self.provider_info.id.clone();
        if self.provider_info.offline.is_some() {
            self.open_modal(Modal::Models(ModelsStep::Offline), None);
            return;
        }
        self.open_modal(
            Modal::Models(ModelsStep::ModelList {
                provider,
                models: vec![],
                selected: 0,
                fetching: true,
            }),
            None,
        );
    }

    fn handle_models_fetched(
        &mut self,
        nonce: u64,
        provider: String,
        result: Result<Vec<String>, FetchError>,
    ) {
        if nonce != self.modal.nonce() {
            return;
        }
        // See `handle_connect_models`: the current fetch has delivered, so stop tracking it.
        self.modal.forget_fetch();
        if !matches!(
            self.modal.current(),
            Some(Modal::Models(ModelsStep::ModelList {
                provider: p,
                fetching: true,
                ..
            })) if *p == provider
        ) {
            return;
        }
        match result {
            Ok(list) => {
                let current = self.provider_info.model.clone();
                let selected = current
                    .as_ref()
                    .and_then(|m| list.iter().position(|x| x == m))
                    .unwrap_or(0);
                if let Some(Modal::Models(ModelsStep::ModelList {
                    models,
                    selected: sel,
                    fetching,
                    ..
                })) = self.modal.current_mut()
                {
                    *models = list;
                    *sel = selected;
                    *fetching = false;
                }
            }
            Err(err) => {
                let message = self.fetch_error_message(&provider, &err);
                // The modal is not a record: `close_models` drops the step, so Esc would erase the
                // only copy of the failure. Log it too, so the user has something to scroll back
                // to and paste when asking for help — the same thing `/key` and `/ask` do.
                self.push_log(message.clone());
                self.modal
                    .replace_step(Modal::Models(if err.class.needs_credentials() {
                        ModelsStep::Credentials {
                            provider,
                            error: message,
                        }
                    } else {
                        ModelsStep::Manual {
                            provider,
                            input: String::new(),
                            error: Some(message),
                        }
                    }));
            }
        }
    }

    /// Render a failed fetch for the user. A missing key already reads as a complete sentence
    /// naming the provider, so wrapping it would produce "openai rejected the credential: No API
    /// key for openai".
    fn fetch_error_message(&self, provider: &str, err: &FetchError) -> String {
        match err.class {
            FetchFailure::MissingKey => err.message.clone(),
            FetchFailure::Auth => self.t_with(
                "models.auth_rejected",
                &[("provider", provider), ("error", &err.message)],
            ),
            FetchFailure::Fetch => self.t_with("connect.fetch_error", &[("error", &err.message)]),
        }
    }

    /// The single key seam for every modal: Ctrl-C quits, the modal's own pure transition decides
    /// the rest, and a step that begins waiting on a model list starts exactly one fetch.
    ///
    /// A retry needs no transition of its own: a failure step's `fetch_target` is `None` and the
    /// step it retries into names one, so the `None -> Some` rule below fires the fetch and
    /// `replace_step` invalidates whatever was still in flight.
    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        // Cloning releases the borrow on `self.modal` so the arms below can take `&mut self`.
        let Some(current) = self.modal.current().cloned() else {
            return false;
        };

        // Blank Enter on the connect key-entry step is a status message, not a transition.
        if matches!(&current, Modal::Connect(ConnectStep::KeyEntry { input, .. })
            if input.trim().is_empty())
            && key.code == KeyCode::Enter
        {
            self.status = self.t("status.key_empty").to_string();
            return false;
        }

        let transition = current.next(key);

        // Storing the typed key needs `self.store`, so it cannot live in the pure transition.
        let mut fetch_key = None;
        if let (
            Modal::Connect(ConnectStep::KeyEntry {
                provider, input, ..
            }),
            // `fetching: true` is pinned alongside `from_key: true` because it is what makes the
            // step name a `fetch_target`. If the two ever diverge, writing the key here while no
            // fetch starts would strand the user on a list that never loads.
            ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                from_key: true,
                fetching: true,
                ..
            })),
        ) = (&current, &transition)
        {
            let key_value = input.trim().to_string();
            if let Err(e) = self.store.set(provider, &key_value) {
                let err = e.to_string();
                self.error = Some(self.t_with(
                    "status.key_failed",
                    &[("provider", provider.as_str()), ("error", &err)],
                ));
                return false;
            }
            fetch_key = Some(key_value);
        }

        let before = current
            .fetch_target()
            .map(|(provider, sink)| (provider.to_string(), sink));
        match transition {
            ModalTransition::Close => self.modal.close(),
            ModalTransition::Apply(apply) => self.apply_and_close_modal(apply),
            ModalTransition::Step(next) => {
                let after = next
                    .fetch_target()
                    .map(|(provider, sink)| (provider.to_string(), sink));
                // Stepping away from a fetch cancels and invalidates it, inside `replace_step`:
                // Esc out of a fetching connect list steps *back* rather than closing, so the
                // request — and the API key in its headers — would otherwise outlive the
                // "Esc: cancel" the footer promises.
                self.modal.replace_step(next);
                // Only a step that *begins* waiting on a list starts a fetch. A step that keeps
                // waiting on the same one — arrow keys on a list still loading — must not respawn
                // it: that would discard the in-flight result and, on the key-entry path, refetch
                // without the key the user just typed.
                if let Some((provider, sink)) = after.filter(|a| Some(a) != before.as_ref()) {
                    self.begin_model_fetch(provider, sink, fetch_key);
                }
            }
        }
        false
    }

    fn set_model(&mut self, model: &str) {
        let active = self.provider_info.id.clone();
        if !is_valid_provider(&active) {
            self.error = Some(self.t("status.model_unsupported").to_string());
            return;
        }
        // `/model <id>` is a blindly typed id by definition — nothing verified it against the
        // provider's list.
        self.persist_model(active, model.to_string(), false);
    }

    fn list_keys(&mut self) {
        let mut parts = Vec::new();
        for name in REMOTE_IDS {
            parts.push(format!("{name}: {}", self.key_status_label(name)));
        }
        self.push_log(self.t_with("key.list", &[("list", &parts.join(", "))]));
    }

    fn begin_key_entry(&mut self, provider: String) {
        if !takes_key(&provider) {
            self.error = Some(self.t_with("status.key_unsupported", &[("provider", &provider)]));
            return;
        }
        self.key_return = self.mode;
        self.key_target = Some(provider);
        self.key_input.clear();
        self.mode = Mode::Key;
    }

    fn clear_key(&mut self, provider: &str) {
        if !takes_key(provider) {
            self.error = Some(self.t_with("status.key_unsupported", &[("provider", provider)]));
            return;
        }
        match self.store.delete(provider) {
            Ok(()) => {
                self.rebuild_provider();
                self.status = self.t_with("status.key_cleared", &[("provider", provider)]);
            }
            Err(e) => {
                let error = e.to_string();
                self.error = Some(self.t_with(
                    "status.key_failed",
                    &[("provider", provider), ("error", &error)],
                ));
            }
        }
    }

    fn key_status_label(&self, provider: &str) -> String {
        match crate::selection::key_status(provider, self.store.as_ref()) {
            crate::selection::KeyStatus::Env => self.t("provider.key.env").to_string(),
            crate::selection::KeyStatus::Keyring => self.t("provider.key.keyring").to_string(),
            crate::selection::KeyStatus::None => self.t("provider.key.none").to_string(),
        }
    }

    /// Run an `/ask` completion off the UI loop so a slow provider never blocks input.
    fn ask(&mut self, prompt: &str) {
        let provider = self.provider.clone();
        let events = self.events.clone();
        let prompt = prompt.to_string();
        tokio::spawn(async move {
            let result = provider.complete(CompleteRequest { prompt }).await;
            let message = match result {
                Ok(resp) => Ok(resp.text),
                Err(e) => Err(e.to_string()),
            };
            let _ = events.send(UiEvent::Completion(message));
        });
    }

    /// Begin the browser-based device login and poll for approval.
    async fn start_device_login(&mut self) {
        self.status = self.t("status.requesting_device_code").to_string();
        match self.api.device().await {
            Ok(resp) => {
                self.device_nonce += 1;
                let nonce = self.device_nonce;
                self.device_user_code = Some(resp.user_code);
                self.device_verification_uri = Some(resp.verification_uri);
                self.mode = Mode::Device;
                self.status = self.t("status.waiting_approval").to_string();

                let _ = browser::open_browser(&resp.verification_uri_complete);

                let api = self.api.clone();
                let events = self.events.clone();
                let device_code = resp.device_code;
                let interval = std::time::Duration::from_secs(resp.interval.max(1));
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(interval).await;
                        match api.device_token(&device_code).await {
                            Ok(auth) => {
                                let _ = events.send(UiEvent::Device {
                                    nonce,
                                    result: Ok(auth),
                                });
                                return;
                            }
                            Err(e) if e.code == "authorization_pending" => continue,
                            Err(e) => {
                                let _ = events.send(UiEvent::Device {
                                    nonce,
                                    result: Err(e),
                                });
                                return;
                            }
                        }
                    }
                });
            }
            Err(e) => {
                self.error = Some(self.error_text(&e.code, &e.message));
                self.status = self.t("status.device_failed").to_string();
            }
        }
    }

    async fn handle_device_result(&mut self, nonce: u64, result: Result<AuthResponse, ApiError>) {
        if nonce != self.device_nonce {
            return;
        }
        self.device_user_code = None;
        self.device_verification_uri = None;
        match result {
            Ok(auth) => {
                let session = Session {
                    token: auth.token,
                    expires_at: auth.expires_at,
                    email: auth.user.email,
                    display_name: auth.user.display_name,
                };
                let _ = session.save();
                self.enter(session).await;
            }
            Err(e) => {
                self.mode = Mode::SignIn;
                // Ctrl-P is reachable from `Mode::Device`, so an overlay may be open over the
                // screen this result is replacing.
                self.dismiss_modals();
                self.error = Some(self.error_text(&e.code, &e.message));
                self.status = self.t("status.device_failed").to_string();
            }
        }
    }

    async fn submit(&mut self) {
        self.error = None;
        match self.mode {
            Mode::SignIn => match self.focus {
                Focus::Email => {
                    if !self.email.is_empty() {
                        self.focus = Focus::Code;
                    }
                }
                Focus::Code => {
                    if self.email.is_empty() {
                        self.error = Some(self.t("status.email_required").to_string());
                        return;
                    }
                    if self.code.is_empty() {
                        self.error = Some(self.t("status.code_required").to_string());
                        return;
                    }
                    self.status = self.t("status.signing_in").to_string();
                    match self.api.login(&self.email, &self.code).await {
                        Ok(auth) => {
                            let session = Session {
                                token: auth.token,
                                expires_at: auth.expires_at,
                                email: auth.user.email,
                                display_name: auth.user.display_name,
                            };
                            let _ = session.save();
                            self.enter(session).await;
                        }
                        Err(e) => self.error = Some(self.error_text(&e.code, &e.message)),
                    }
                }
                Focus::Name => {}
            },
            Mode::Register => match self.focus {
                Focus::Email => {
                    if self.email.is_empty() {
                        self.error = Some(self.t("status.email_required").to_string());
                        return;
                    }
                    self.focus = Focus::Name;
                }
                Focus::Name => {
                    if self.email.is_empty() {
                        self.error = Some(self.t("status.email_required").to_string());
                        return;
                    }
                    self.status = self.t("status.creating_account").to_string();
                    match self.api.register(&self.email, Some(&self.name)).await {
                        Ok(resp) => {
                            self.setup_token = Some(resp.setup_token);
                            self.secret = Some(resp.secret);
                            self.otpauth_url = Some(resp.otpauth_url);
                            self.mode = Mode::RegisterCode;
                            self.focus = Focus::Code;
                            self.code.clear();
                            self.status = self.t("status.scan_confirm").to_string();
                        }
                        Err(e) => self.error = Some(self.error_text(&e.code, &e.message)),
                    }
                }
                Focus::Code => {}
            },
            Mode::RegisterCode => {
                if self.code.is_empty() {
                    self.error = Some(self.t("status.code_required").to_string());
                    return;
                }
                let setup_token = self.setup_token.clone().unwrap_or_default();
                self.status = self.t("status.confirming").to_string();
                match self.api.register_confirm(&setup_token, &self.code).await {
                    Ok(auth) => {
                        let session = Session {
                            token: auth.token,
                            expires_at: auth.expires_at,
                            email: auth.user.email,
                            display_name: auth.user.display_name,
                        };
                        let _ = session.save();
                        self.enter(session).await;
                    }
                    Err(e) => self.error = Some(self.error_text(&e.code, &e.message)),
                }
            }
            Mode::Device => {}
            Mode::Connected => {}
            Mode::Engine => {}
            Mode::Key => {}
        }
    }

    /// Move into the connected state and open the WebSocket.
    async fn enter(&mut self, session: Session) {
        self.session = Some(session.clone());
        self.mode = Mode::Connected;
        // Reached asynchronously from `handle_device_result`, where an overlay opened while the
        // device code was pending would otherwise outlive the screen it was opened over.
        self.dismiss_modals();
        self.focus = Focus::Code;
        self.code.clear();
        self.name.clear();
        self.setup_token = None;
        self.secret = None;
        self.otpauth_url = None;
        self.error = None;
        self.status = self.t("status.connecting").to_string();

        let events = self.events.clone();
        let config = self.config.clone();
        match ws::connect(&config, &session.token, &events).await {
            Ok(tx) => {
                self.ws_tx = Some(tx);
                self.status = self.t_with("status.connected_as", &[("email", &session.email)]);
            }
            Err(e) => {
                let error = e.to_string();
                self.error = Some(self.t_with("status.connect_failed", &[("error", &error)]));
                self.status = self.t("status.ws_failed").to_string();
            }
        }
    }

    async fn sign_out(&mut self) {
        // Before the logout await, not after. `self.api` is a `reqwest::Client::new()` with no
        // timeout, so a server that never answers `logout` would otherwise delay cancellation of
        // the key-bearing model fetch indefinitely — the exact window the abort exists to close.
        self.dismiss_modals();
        if let Some(session) = &self.session {
            let _ = self.api.logout(&session.token).await;
        }
        let _ = Session::clear();
        self.session = None;
        self.ws_tx = None;
        self.mode = Mode::SignIn;
        self.focus = Focus::Email;
        self.code.clear();
        self.log.clear();
        self.pongs = 0;
        self.status = self.t("status.signed_out").to_string();
        self.error = None;
    }

    fn handle_server(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::Ready { user } => {
                self.push_log(self.t_with(
                    "status.ready",
                    &[("name", &user.display_name), ("email", &user.email)],
                ));
            }
            ServerMessage::Pong { nonce } => {
                self.pongs += 1;
                let nonce = nonce.to_string();
                self.push_log(self.t_with("status.pong", &[("nonce", &nonce)]));
            }
            ServerMessage::Error { code, message } => {
                self.push_log(format!("[{code}] {message}"));
                if code == "ws_closed" {
                    self.ws_tx = None;
                    let text = self.error_text(&code, &message);
                    self.status = self.t_with("status.disconnected", &[("reason", &text)]);
                    self.session = None;
                    self.mode = Mode::SignIn;
                    self.focus = Focus::Email;
                    self.dismiss_modals();
                    self.error = Some(text);
                }
            }
        }
    }

    fn ping(&mut self) {
        if let Some(tx) = self.ws_tx.clone() {
            self.nonce += 1;
            let nonce = self.nonce.to_string();
            self.push_log(self.t_with("status.ping", &[("nonce", &nonce)]));
            let _ = tx.send(ClientMessage::Ping { nonce });
        }
    }

    fn handle_completion(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => self.push_log(text),
            Err(e) => self.push_log(format!("[ask] {e}")),
        }
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(line);
        while self.log.len() > LOG_CAPACITY {
            self.log.pop_front();
        }
    }

    fn type_char(&mut self, c: char) {
        self.error = None;
        match (self.mode, self.focus) {
            (Mode::SignIn, Focus::Email) => self.email.push(c),
            (Mode::SignIn, Focus::Code) => self.code.push(c),
            (Mode::Register, Focus::Email) => self.email.push(c),
            (Mode::Register, Focus::Name) => self.name.push(c),
            (Mode::RegisterCode, Focus::Code) => self.code.push(c),
            _ => {}
        }
    }

    fn backspace(&mut self) {
        match (self.mode, self.focus) {
            (Mode::SignIn, Focus::Email) => {
                self.email.pop();
            }
            (Mode::SignIn, Focus::Code) => {
                self.code.pop();
            }
            (Mode::Register, Focus::Email) => {
                self.email.pop();
            }
            (Mode::Register, Focus::Name) => {
                self.name.pop();
            }
            (Mode::RegisterCode, Focus::Code) => {
                self.code.pop();
            }
            _ => {}
        }
    }

    fn cycle_focus(&mut self) {
        match self.mode {
            Mode::SignIn => {
                self.focus = match self.focus {
                    Focus::Email => Focus::Code,
                    _ => Focus::Email,
                }
            }
            Mode::Register => {
                self.focus = match self.focus {
                    Focus::Email => Focus::Name,
                    _ => Focus::Email,
                }
            }
            Mode::RegisterCode => self.focus = Focus::Code,
            Mode::Device => {}
            Mode::Connected => {}
            Mode::Engine => {}
            Mode::Key => {}
        }
    }

    fn field(label: &str, value: &str, focused: bool) -> Line<'static> {
        let marker = if focused { "> " } else { "  " };
        let label_style = if focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let mut spans = vec![
            Span::styled(format!("{marker}{label}: "), label_style),
            Span::raw(value.to_string()),
        ];
        if focused {
            spans.push(Span::styled("_", label_style));
        }
        Line::from(spans)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(frame.area());

        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" light-factory · {}", self.status),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );

        // Built before the base screen so the same value decides whether that screen is drawn at
        // all: a modal covers the base exactly when it renders as a full-area pane.
        let modal_view = self.modal.current().map(|modal| {
            let ctx = ModalContext {
                locale: self.config.lang,
                error: self.error.as_deref(),
                offline: self.provider_info.offline.as_ref(),
            };
            (modal.hint_key(), modal.view(&ctx))
        });

        if !modal_view
            .as_ref()
            .is_some_and(|(_, view)| view.covers_base())
        {
            match self.mode {
                Mode::SignIn => self.draw_signin(frame, chunks[1]),
                Mode::Register => self.draw_register(frame, chunks[1]),
                Mode::RegisterCode => self.draw_register_code(frame, chunks[1]),
                Mode::Device => self.draw_device(frame, chunks[1]),
                Mode::Connected => self.draw_connected(frame, chunks[1]),
                Mode::Engine => self.draw_engine(frame, chunks[1]),
                Mode::Key => self.draw_key(frame, chunks[1]),
            }
        }

        let modal_hint = modal_view.and_then(|(hint, view)| {
            crate::modal::draw_modal(frame, chunks[1], view);
            hint
        });

        let hints = if self.command_mode {
            format!("> {}", self.command)
        } else if let Some(hint) = modal_hint {
            self.t(hint).to_string()
        } else if self.mode == Mode::Device {
            self.t("hint.device_cancel").to_string()
        } else {
            self.t("hint.help").to_string()
        };
        frame.render_widget(
            Paragraph::new(hints).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn draw_signin(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(Span::styled(
            self.t("screen.sign_in"),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(""));
        lines.push(Self::field(
            self.t("field.email"),
            &self.email,
            self.focus == Focus::Email,
        ));
        lines.push(Self::field(
            self.t("field.code"),
            &self.code,
            self.focus == Focus::Code,
        ));
        lines.push(Line::from(""));
        lines.push(match &self.error {
            Some(err) => Line::from(Span::styled(err.clone(), Style::default().fg(Color::Red))),
            None => Line::from(Span::styled(
                self.t("hint.sign_in"),
                Style::default().fg(Color::DarkGray),
            )),
        });
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_register(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(Span::styled(
            self.t("screen.create_account"),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(""));
        lines.push(Self::field(
            self.t("field.email"),
            &self.email,
            self.focus == Focus::Email,
        ));
        lines.push(Self::field(
            self.t("field.name"),
            &self.name,
            self.focus == Focus::Name,
        ));
        lines.push(Line::from(""));
        lines.push(match &self.error {
            Some(err) => Line::from(Span::styled(err.clone(), Style::default().fg(Color::Red))),
            None => Line::from(Span::styled(
                self.t("hint.register"),
                Style::default().fg(Color::DarkGray),
            )),
        });
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_register_code(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(Span::styled(
            self.t("screen.complete_registration"),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(""));
        lines.push(Line::from(Span::raw(
            self.t_with("hint.account", &[("email", &self.email)]),
        )));
        if let Some(url) = &self.otpauth_url {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                self.t("hint.open_url"),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::raw(url.clone())));
        }
        if let Some(secret) = &self.secret {
            lines.push(Line::from(vec![
                Span::styled(
                    self.t("hint.manual_secret"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(secret.clone()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Self::field(
            self.t("field.code"),
            &self.code,
            self.focus == Focus::Code,
        ));
        lines.push(Line::from(""));
        if let Some(err) = &self.error {
            lines.push(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(Color::Red),
            )));
        }
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_device(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(Span::styled(
            self.t("screen.device_login"),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(""));
        lines.push(Line::from(self.t("hint.device_line1")));
        lines.push(Line::from(self.t("hint.device_line2")));
        lines.push(Line::from(""));
        lines.push(Line::from(self.t("hint.device_visit")));
        if let Some(url) = &self.device_verification_uri {
            lines.push(Line::from(Span::styled(
                url.clone(),
                Style::default().fg(Color::Cyan),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            self.t("hint.device_code_filled"),
            Style::default().fg(Color::DarkGray),
        )));
        if let Some(code) = &self.device_user_code {
            lines.push(Line::from(Span::styled(
                code.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            self.t("hint.device_waiting"),
            Style::default().fg(Color::DarkGray),
        )));
        if let Some(err) = &self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(Color::Red),
            )));
        }
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_connected(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        let info = match &self.session {
            Some(s) => {
                let pongs = self.pongs.to_string();
                let mut provider = self.provider_info.display();
                let reason = self.provider_info.reason(self.config.lang);
                if !reason.is_empty() {
                    provider = format!("{provider} · {reason}");
                }
                self.t_with(
                    "info.connected",
                    &[
                        ("name", &s.display_name),
                        ("email", &s.email),
                        ("pongs", &pongs),
                        ("provider", &provider),
                    ],
                )
            }
            None => self.t("hint.not_signed_in").to_string(),
        };
        frame.render_widget(
            Paragraph::new(info).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            ),
            chunks[0],
        );

        let items: Vec<ListItem> = if self.log.is_empty() {
            vec![ListItem::new(Span::styled(
                self.t("hint.no_messages"),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.log.iter().map(|l| ListItem::new(l.clone())).collect()
        };
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", self.t("title.activity"))),
        );
        frame.render_widget(list, chunks[1]);
    }

    fn draw_engine(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(area);

        let items: Vec<ListItem> = if self.engine_log.is_empty() {
            vec![ListItem::new(Span::styled(
                self.t("hint.no_messages"),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.engine_log
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect()
        };
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", self.t("title.engine"))),
        );
        frame.render_widget(list, chunks[0]);

        let footer_text = match &self.pending {
            Some((_, prompt)) => prompt.clone(),
            None => format!("> {}", self.engine_prompt),
        };
        frame.render_widget(
            Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL)),
            chunks[1],
        );
    }

    fn draw_key(&self, frame: &mut Frame, area: Rect) {
        let provider = self.key_target.as_deref().unwrap_or("");
        let masked = mask(&self.key_input);
        let mut lines = vec![
            Line::from(Span::styled(
                self.t_with("status.key_enter", &[("provider", provider)]),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Self::field(self.t("field.key"), &masked, true),
            Line::from(""),
            Line::from(Span::styled(
                self.t("hint.key"),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        if let Some(err) = &self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(Color::Red),
            )));
        }
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" light-factory "),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

/// Run the terminal UI until the user quits.
pub async fn run(
    config: Config,
    provider: Arc<dyn Provider>,
    provider_info: ProviderInfo,
    store: Arc<dyn CredentialStore>,
    settings: SettingsHandle,
    prefilled_email: Option<String>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (events, mut event_rx) = mpsc::unbounded_channel::<UiEvent>();
    let mut app = App::new(
        config,
        provider,
        provider_info,
        store,
        settings,
        prefilled_email,
        events.clone(),
    );

    // Restore a saved session if it is still valid; otherwise go straight into
    // the browser-based device login.
    let restored = match Session::load() {
        Some(session) => match app.api.me(&session.token).await {
            Ok(_) => {
                app.enter(session).await;
                true
            }
            Err(_) => {
                let _ = Session::clear();
                false
            }
        },
        None => false,
    };
    if !restored {
        app.start_device_login().await;
    }

    let input_events = events.clone();
    tokio::spawn(async move {
        let mut stream = crossterm::event::EventStream::new();
        while let Some(Ok(ev)) = stream.next().await {
            if let Event::Key(key) = ev
                && input_events.send(UiEvent::Key(key)).is_err()
            {
                break;
            }
        }
    });

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;

    let result: anyhow::Result<()> = loop {
        tokio::select! {
            Some(ev) = event_rx.recv() => {
                match ev {
                    UiEvent::Key(key) => {
                        if app.handle_key(key).await {
                            break Ok(());
                        }
                    }
                    UiEvent::Server(msg) => app.handle_server(msg),
                    UiEvent::Device { nonce, result } => {
                        app.handle_device_result(nonce, result).await
                    }
                    UiEvent::Completion(result) => app.handle_completion(result),
                    UiEvent::Engine(event) => app.handle_engine_event(event),
                    UiEvent::EngineDropped(n) => app.handle_engine_dropped(n),
                    UiEvent::ConnectModels {
                        nonce,
                        provider,
                        result,
                    } => app.handle_connect_models(nonce, provider, result),
                    UiEvent::ModelsFetched {
                        nonce,
                        provider,
                        result,
                    } => app.handle_models_fetched(nonce, provider, result),
                    UiEvent::KeyProbed {
                        nonce,
                        provider,
                        result,
                    } => app.handle_key_probed(nonce, provider, result),
                }
            }
            _ = tick.tick() => {
                ticks += 1;
                if app.ws_tx.is_some() && ticks.is_multiple_of(KEEPALIVE_SECONDS) {
                    app.ping();
                }
            }
        }
        terminal.draw(|f| app.draw(f))?;
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Map an engine-mode letter key to an approval answer while an approval is pending.
/// `a`/`d` approve/deny only when `pending` is set; otherwise they are ordinary input.
fn engine_approval_key(c: char, pending: bool) -> Option<bool> {
    if !pending {
        return None;
    }
    match c {
        'a' => Some(true),
        'd' => Some(false),
        _ => None,
    }
}

/// One step of the engine-event forwarding loop, decoded from a `broadcast::Receiver::recv`
/// result. `Lagged(n)` continues the loop (surfacing a "dropped n events" notice); only a
/// closed channel ends forwarding.
enum EngineForward {
    Event(EngineEvent),
    Dropped(u64),
    Stop,
}

fn engine_forward_step(
    result: Result<EngineEvent, tokio::sync::broadcast::error::RecvError>,
) -> EngineForward {
    match result {
        Ok(event) => EngineForward::Event(event),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => EngineForward::Dropped(n),
        Err(tokio::sync::broadcast::error::RecvError::Closed) => EngineForward::Stop,
    }
}

/// Parse an `/ask <prompt>` command into the prompt, or `None` when the command is not an
/// `/ask` with a non-empty prompt. `/ask` and `/ask   ` (empty prompt) are `None` so the caller
/// can show a usage hint; `/askhello` (no word boundary) and other commands are also `None`.
fn parse_ask_command(command: &str) -> Option<&str> {
    let rest = command.trim().strip_prefix("/ask")?;
    let boundary = rest.chars().next().map(char::is_whitespace).unwrap_or(true);
    if !boundary {
        return None;
    }
    let prompt = rest.trim();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt)
    }
}

use crate::selection::REMOTE_IDS;

/// Every provider the connect modal can offer.
const PROVIDER_NAMES: [&str; 5] = ["anthropic", "openai", "gemini", "deepseek", "ollama"];

fn is_valid_provider(name: &str) -> bool {
    PROVIDER_NAMES.contains(&name)
}

/// A parsed `/key` command.
enum KeyCommand {
    List,
    Set(String),
    Clear(String),
}

/// Parse a `/connect` command: `true` for `/connect` (optionally followed by whitespace), `false`
/// otherwise — including `/connectX` (no word boundary), mirroring `/ask`.
fn parse_connect_command(command: &str) -> bool {
    command
        .trim()
        .strip_prefix("/connect")
        .map(word_boundary)
        .unwrap_or(false)
}

/// Parse a `/models` command: `true` for `/models` (optionally followed by whitespace), `false`
/// otherwise — including `/modelsX` (no word boundary), mirroring `/connect`.
fn parse_models_command(command: &str) -> bool {
    command
        .trim()
        .strip_prefix("/models")
        .map(word_boundary)
        .unwrap_or(false)
}

/// Parse a `/model` command: `Some(Some(id))` for `/model <id>`, `Some(None)` for a bare `/model`
/// (empty arg), or `None` when the command is not `/model`.
fn parse_model_command(command: &str) -> Option<Option<&str>> {
    let rest = command.strip_prefix("/model")?;
    if !word_boundary(rest) {
        return None;
    }
    let arg = rest.trim();
    if arg.is_empty() {
        Some(None)
    } else {
        Some(Some(arg))
    }
}

/// Parse a `/key` command: bare → `List`, `/key <provider>` → `Set`, `/key <provider> clear` →
/// `Clear`. `None` when the command is not `/key`.
fn parse_key_command(command: &str) -> Option<KeyCommand> {
    let rest = command.strip_prefix("/key")?;
    if !word_boundary(rest) {
        return None;
    }
    let arg = rest.trim();
    if arg.is_empty() {
        return Some(KeyCommand::List);
    }
    if let Some(provider) = arg.strip_suffix(" clear").map(str::trim)
        && !provider.is_empty()
    {
        return Some(KeyCommand::Clear(provider.to_string()));
    }
    Some(KeyCommand::Set(arg.to_string()))
}

/// True when `rest` begins at a word boundary (empty, or starts with whitespace) — so `/askhello`,
/// `/providerX`, etc. do not match their commands.
fn word_boundary(rest: &str) -> bool {
    rest.chars().next().map(char::is_whitespace).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        ApiError, App, ConnectStep, EngineForward, FetchError, FetchFailure, FetchSink, KeyCommand,
        Modal, Mode, ModelsStep, ProviderRow, Session, UiEvent, engine_approval_key,
        engine_forward_step, fetch_error, parse_ask_command, parse_connect_command,
        parse_key_command, parse_model_command, parse_models_command,
    };

    use crate::config::Config;

    use crate::provider::ProviderInfo;

    use crate::settings::{Settings, SettingsHandle};

    use light_factory_protocol::session::{Event as EngineEvent, EventKind, SessionId};

    use light_factory_protocol::wire::ServerMessage;

    use light_factory_providers::{LocalProvider, OfflineReason, Provider};

    use light_factory_tui::credentials::{CredentialStore, MemStore};

    use light_factory_tui::i18n::Locale;

    use ratatui::{Frame, Terminal};

    use tokio::sync::broadcast::error::RecvError;

    use tokio::sync::mpsc;

    fn test_app_with_store(store: Arc<dyn CredentialStore>) -> App {
        let config = Config::from_url("http://localhost:8080").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(LocalProvider::new());
        let provider_info = ProviderInfo {
            id: "local".to_string(),
            model: None,
            offline: None,
            selected_by: None,
            warnings: Vec::new(),
        };
        let (events, _rx) = mpsc::unbounded_channel::<UiEvent>();
        // Isolation by construction: no test may ever write the developer's real config.json.
        App::new(
            config,
            provider,
            provider_info,
            store,
            SettingsHandle {
                settings: Settings::default(),
                path: temp_settings_path(),
            },
            None,
            events,
        )
    }

    fn test_app() -> App {
        test_app_with_store(Arc::new(MemStore::new()))
    }

    /// A unique settings file under the temp dir, so no test ever touches the developer's real
    /// `config.json`. Every `test_app*` gets its own, so parallel tests cannot collide.
    fn temp_settings_path() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("light-factory-app-{}-{n}.json", std::process::id()))
    }

    /// Removes the settings file it names when the test ends, panic or not.
    struct TempSettings(std::path::PathBuf);

    impl Drop for TempSettings {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// A store whose `set` always fails, for exercising the keyring-failure branch.
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn get(&self, _provider: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        fn set(&self, _provider: &str, _key: &str) -> anyhow::Result<()> {
            anyhow::bail!("keyring unavailable")
        }

        fn delete(&self, _provider: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// The popup contents as one whitespace-collapsed string, so an assertion can check that a
    /// wrapped line survived *in full* without having to predict where the wrapper broke it.
    fn flatten(screen: &str) -> String {
        screen
            .chars()
            .map(|c| {
                if "\u{2502}\u{250c}\u{2510}\u{2514}\u{2518}\u{2500}".contains(c) {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
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

    /// Open a modal directly on the host, bypassing `App::open_modal` (which would spawn a fetch
    /// outside a runtime), and return the host's nonce afterwards.
    ///
    /// That is the nonce a *result* must carry to be accepted while nothing else has moved — not
    /// the nonce a real fetch would carry, which is one higher: production `open_modal` bumps once
    /// to open and again in `begin_model_fetch`.
    fn open(app: &mut App, modal: Modal) -> u64 {
        app.modal.open(modal);
        app.modal.nonce()
    }

    /// The open connect step, for tests that assert on it.
    fn connect_step(app: &App) -> Option<&ConnectStep> {
        match app.modal.current() {
            Some(Modal::Connect(step)) => Some(step),
            _ => None,
        }
    }

    /// The open models step, for tests that assert on it.
    fn models_step(app: &App) -> Option<&ModelsStep> {
        match app.modal.current() {
            Some(Modal::Models(step)) => Some(step),
            _ => None,
        }
    }

    fn connect_model_list_step(models: Vec<String>, fetching: bool) -> ConnectStep {
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

    fn fetch_err(class: FetchFailure, message: &str) -> FetchError {
        FetchError {
            class,
            message: message.to_string(),
        }
    }

    #[test]
    fn approval_keys_only_fire_while_a_prompt_is_pending() {
        assert_eq!(engine_approval_key('a', true), Some(true));
        assert_eq!(engine_approval_key('d', true), Some(false));
        assert_eq!(engine_approval_key('a', false), None);
        assert_eq!(engine_approval_key('d', false), None);
        assert_eq!(engine_approval_key('x', true), None);
    }

    #[test]
    fn a_lagged_broadcast_continues_instead_of_stopping() {
        assert!(matches!(
            engine_forward_step(Err(RecvError::Lagged(7))),
            EngineForward::Dropped(7)
        ));
        assert!(matches!(
            engine_forward_step(Err(RecvError::Closed)),
            EngineForward::Stop
        ));
    }

    #[test]
    fn a_received_event_is_forwarded() {
        let event = EngineEvent {
            seq: 1,
            session: SessionId::new(),
            kind: EventKind::Log {
                message: "hi".into(),
            },
        };
        assert!(matches!(
            engine_forward_step(Ok(event)),
            EngineForward::Event(_)
        ));
    }

    #[test]
    fn parses_an_ask_prompt() {
        assert_eq!(parse_ask_command("/ask hello"), Some("hello"));
    }

    #[test]
    fn rejects_an_empty_ask() {
        assert_eq!(parse_ask_command("/ask"), None);
        assert_eq!(parse_ask_command("/ask   "), None);
    }

    #[test]
    fn rejects_other_commands() {
        assert_eq!(parse_ask_command("/auth/login"), None);
        assert_eq!(parse_ask_command("/askhello"), None);
    }

    #[test]
    fn parses_connect_command() {
        assert!(parse_connect_command("/connect"));
        assert!(parse_connect_command("/connect   "));
        assert!(!parse_connect_command("/connectx"));
        assert!(!parse_connect_command("/provider"));
        assert!(!parse_connect_command("/ask hello"));
    }

    #[test]
    fn handle_connect_models_ignores_stale_nonces() {
        let mut app = test_app();
        let nonce = open(
            &mut app,
            Modal::Connect(connect_model_list_step(vec![], true)),
        );
        app.handle_connect_models(
            nonce - 1,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string()]),
        );
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::ModelList {
                fetching: true,
                models,
                ..
            }) if models.is_empty()
        ));
    }

    #[test]
    fn handle_connect_models_fills_models_for_a_matching_nonce() {
        let mut app = test_app();
        let nonce = open(
            &mut app,
            Modal::Connect(connect_model_list_step(vec![], true)),
        );
        app.handle_connect_models(
            nonce,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]),
        );
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::ModelList {
                fetching: false,
                error: None,
                models,
                ..
            }) if *models == vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
        ));
    }

    #[test]
    fn handle_connect_models_surfaces_a_fetch_error() {
        let mut app = test_app();
        let nonce = open(
            &mut app,
            Modal::Connect(connect_model_list_step(vec![], true)),
        );
        app.handle_connect_models(nonce, "openai".to_string(), Err("bad key".to_string()));
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::ModelList {
                fetching: false,
                error: Some(_),
                ..
            })
        ));
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

    /// Render the whole app to an off-screen terminal and return it as text, so modal rendering
    /// can be asserted without a real terminal.
    fn render(app: &mut App, width: u16, height: u16) -> String {
        draw_to_text(width, height, |frame| app.draw(frame))
    }

    #[test]
    fn models_modal_renders_its_own_header() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(models_list_step(vec!["gpt-4o".to_string()], false)),
        );
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("Select a model"), "{screen}");
        assert!(screen.contains("gpt-4o"), "{screen}");
        assert!(
            screen.contains("Activity"),
            "a popup floats over the connected screen; drawing it instead of the screen leaves the \
             base blank:\n{screen}"
        );
    }

    /// The converse of the assertion above, and the other half of what `covers_base` decides:
    /// help is a full-area pane, and `draw_full_screen` deliberately does not `Clear`, so the
    /// screen underneath must not be drawn at all.
    #[test]
    fn help_replaces_the_screen_underneath_it() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        app.open_help();
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("Help"), "{screen}");
        assert!(
            !screen.contains("Activity"),
            "help covers the base screen, which is not cleared behind it:\n{screen}"
        );
    }

    /// The whole point of the credential step: it must point at the commands that can actually
    /// fix the problem, and must not offer a model-id box.
    ///
    /// Built through `handle_models_fetched` from a real `reqwest` 401 rather than from a
    /// hand-written 37-character string no code path can produce. The real message is 105
    /// characters and wraps to two rows against the 58-column inner width, which is exactly the
    /// case the old popup sizing clipped — so on every real 401 the remedy was off screen.
    #[tokio::test]
    async fn the_credentials_step_renders_the_remedy_and_no_input_box() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));

        let err = fetch_error("openai", &status_error(401).await);
        assert_eq!(
            err.class,
            FetchFailure::Auth,
            "a 401 is a credential failure"
        );
        let cause = err.message.clone();
        app.handle_models_fetched(nonce, "openai".to_string(), Err(err));

        let screen = render(&mut app, 80, 20);
        assert!(
            screen.contains("/connect"),
            "the remedy is clipped:\n{screen}"
        );
        assert!(
            screen.contains("/key openai"),
            "the remedy is clipped:\n{screen}"
        );
        assert!(
            screen.contains("/model <id>"),
            "a misclassified 401 needs an escape hatch:\n{screen}"
        );
        assert!(
            flatten(&screen).contains(&flatten(&format!(
                "openai rejected the credential: {cause}"
            ))),
            "the cause must be rendered in full, not clipped:\n{screen}"
        );
        assert!(
            !screen.contains("Type a model id"),
            "typing an id cannot repair a credential:\n{screen}"
        );
        assert!(
            !screen.contains("save unverified"),
            "the credential step must not offer to save an id:\n{screen}"
        );
        assert!(
            screen.contains("Ctrl+R: retry"),
            "a 401 from a proxy or a WAF is not a dead end:\n{screen}"
        );
    }

    /// The transport step keeps the manual fallback (#36 AC 6) but must label it as unverified,
    /// advertise the retry key, and — the part that was broken — actually show the input box.
    ///
    /// Built through `handle_models_fetched` from a real refused connection. The old test used a
    /// helper that hardcoded `error: None`, which the `Err` arm never produces, so it dropped the
    /// error line and never exercised the layout the user actually gets: the prompt and the input
    /// row pushed off screen while keystrokes still accumulated and Enter still applied.
    #[tokio::test]
    async fn the_manual_step_labels_itself_unverified_and_offers_a_retry() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));

        let err = fetch_error("openai", &transport_error().await);
        assert_eq!(
            err.class,
            FetchFailure::Fetch,
            "a refused connection is retryable"
        );
        let cause = err.message.clone();
        app.handle_models_fetched(nonce, "openai".to_string(), Err(err));

        // Type into the box the way the user does. What they type must be on screen.
        for c in "o3-mini".chars() {
            app.handle_modal_key(key(KeyCode::Char(c)));
        }
        let screen = render(&mut app, 80, 20);
        assert!(
            screen.contains("Type a model id \u{2014} it won't be checked against openai"),
            "the unverified prompt is clipped:\n{screen}"
        );
        assert!(
            screen.contains("o3-mini"),
            "the user must be able to see what they are typing:\n{screen}"
        );
        assert!(
            flatten(&screen).contains(&flatten(&format!("Couldn't fetch models: {cause}"))),
            "the cause must be rendered in full, not clipped:\n{screen}"
        );
        assert!(screen.contains("Ctrl+R: retry"), "{screen}");
        assert!(screen.contains("save unverified"), "{screen}");
    }

    /// Every other render assertion in this file runs in EN, which is how a 63-column ES footer
    /// shipped hard-truncated against a 58-column inner width, silently costing ES users
    /// "Esc: cerrar".
    #[test]
    fn the_manual_step_footer_is_not_truncated_in_spanish() {
        let mut app = test_app();
        app.config.lang = Locale::Es;
        app.mode = Mode::Connected;
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(FetchFailure::Fetch, "conexi\u{f3}n rechazada")),
        );

        let screen = render(&mut app, 80, 20);
        assert!(
            screen.contains("Esc: cerrar"),
            "the ES footer lost its last key to truncation:\n{screen}"
        );
        assert!(
            screen.contains("Ctrl+R: reintentar"),
            "the ES footer lost its retry key:\n{screen}"
        );
    }

    #[test]
    fn a_long_model_list_keeps_the_selection_and_footer_on_screen() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        let models: Vec<String> = (0..80).map(|i| format!("model-{i:03}")).collect();
        open(
            &mut app,
            Modal::Models(ModelsStep::ModelList {
                provider: "openai".to_string(),
                models,
                selected: 60,
                fetching: false,
            }),
        );

        let screen = render(&mut app, 80, 24);

        assert!(
            screen.contains("> model-060"),
            "the highlighted row scrolled off screen:\n{screen}"
        );
        assert!(
            screen.contains("Enter: select"),
            "the footer scrolled off screen:\n{screen}"
        );
    }

    #[test]
    fn an_empty_list_does_not_advertise_enter() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(&mut app, Modal::Models(models_list_step(vec![], false)));
        let screen = render(&mut app, 80, 20);
        assert!(
            !screen.contains("Enter: select"),
            "Enter is a no-op with nothing to select:\n{screen}"
        );
        assert!(screen.contains("Esc: close"), "{screen}");
    }

    #[test]
    fn the_offline_modal_names_the_actual_reason() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        app.provider_info.offline = Some(OfflineReason::NamedProviderMissingKey {
            selector: "openai".to_string(),
            key: "OPENAI_API_KEY".to_string(),
        });
        open(&mut app, Modal::Models(ModelsStep::Offline));
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("OPENAI_API_KEY"), "{screen}");
    }

    #[test]
    fn a_popup_on_a_tiny_terminal_does_not_panic() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(models_list_step(vec!["gpt-4o".to_string()], false)),
        );
        for (w, h) in [(4u16, 3u16), (10, 4), (80, 5)] {
            let _ = render(&mut app, w, h);
        }
    }

    #[test]
    fn models_fetch_result_is_ignored_when_the_provider_does_not_match() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "anthropic".to_string(),
            Ok(vec!["claude".to_string()]),
        );
        assert!(
            matches!(models_step(&app), Some(ModelsStep::ModelList { fetching: true, models, .. }) if models.is_empty()),
            "a result for another provider must not populate this modal"
        );
    }

    #[test]
    fn models_fetch_result_does_not_clobber_manual_entry() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_manual_step("gpt-4")));
        app.handle_models_fetched(nonce, "openai".to_string(), Ok(vec!["gpt-4o".to_string()]));
        assert!(
            matches!(models_step(&app), Some(ModelsStep::Manual { input, .. }) if input == "gpt-4"),
            "a late result must not discard what the user typed"
        );
    }

    /// The same guard on the `Err` path, which every other stale-result test misses. A superseded
    /// `Err(Auth)` landing on a manual step would wipe half-typed input *and* replace a step that
    /// has an input box with one that does not — the worst version of the clobber.
    #[test]
    fn a_late_failure_does_not_clobber_manual_entry() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_manual_step("gpt-4")));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(FetchFailure::Auth, "401 Unauthorized")),
        );
        assert!(
            matches!(models_step(&app), Some(ModelsStep::Manual { input, .. }) if input == "gpt-4"),
            "a late failure must not discard what the user typed, got {:?}",
            models_step(&app)
        );
    }

    /// The step is not a record: `close_models` drops it, so Esc would erase the only copy of the
    /// failure. The transcript is what the user can scroll back to and paste when asking for help.
    #[test]
    fn a_fetch_failure_is_recorded_in_the_transcript() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(FetchFailure::Auth, "401 Unauthorized")),
        );

        assert!(
            app.log
                .iter()
                .any(|l| l.contains("openai") && l.contains("401 Unauthorized")),
            "the classified failure must outlive the modal: {:?}",
            app.log
        );

        app.modal.close();
        assert!(
            app.log.iter().any(|l| l.contains("401 Unauthorized")),
            "closing the modal must not erase the record: {:?}",
            app.log
        );
    }

    #[test]
    fn an_empty_but_successful_fetch_stays_a_list() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(nonce, "openai".to_string(), Ok(vec![]));
        assert!(
            matches!(
                models_step(&app),
                Some(ModelsStep::ModelList {
                    fetching: false,
                    models,
                    ..
                }) if models.is_empty()
            ),
            "an empty list is not a fetch failure and must not route to manual entry"
        );
    }

    #[tokio::test]
    async fn models_command_opens_a_fetching_list_for_the_active_provider() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        // `local` resolves no key, so the spawned fetch fails offline instead of hitting network.
        app.provider_info.id = "local".to_string();
        app.run_command("/models").await;
        assert!(
            matches!(
                models_step(&app),
                Some(ModelsStep::ModelList { provider, fetching: true, .. }) if provider == "local"
            ),
            "expected a fetching list scoped to provider_info.id, got {:?}",
            models_step(&app)
        );
        assert_ne!(app.modal.nonce(), 0, "the fetch nonce must be bumped");
    }

    /// Spawn a task that never completes, plus a probe that can observe its cancellation after the
    /// `JoinHandle` has been moved into the `App`. Bounded yields rather than a sleep, so the test
    /// is deterministic and makes no assertion about elapsed wall-clock time.
    fn pending_task() -> (tokio::task::JoinHandle<()>, tokio::task::AbortHandle) {
        let handle = tokio::spawn(std::future::pending::<()>());
        let probe = handle.abort_handle();
        (handle, probe)
    }

    async fn settle(probe: &tokio::task::AbortHandle) {
        for _ in 0..32 {
            if probe.is_finished() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Esc on a `/models` list, which routes to `ModalTransition::Close`.
    #[tokio::test]
    async fn closing_the_models_modal_aborts_the_in_flight_fetch() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.modal.track_fetch(handle);

        app.handle_modal_key(key(KeyCode::Esc));

        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "Esc must cancel the request, not just hide the modal: the connection carries the API key"
        );
        assert!(!app.modal.tracks_fetch());
    }

    /// Was `closing_the_connect_modal_aborts_the_in_flight_fetch`, which is now the same call as
    /// the test above — one host, one close. Renamed to cover the other close path instead, which
    /// the one host makes the *only* other one: `Apply` closes a `/connect` list whose fetch may
    /// still be running. There is no per-modal close method left to forget the abort in.
    #[tokio::test]
    async fn applying_from_the_connect_modal_aborts_the_in_flight_fetch() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        let (handle, probe) = pending_task();
        open(
            &mut app,
            Modal::Connect(connect_model_list_step(vec!["gpt-4o".to_string()], false)),
        );
        app.modal.track_fetch(handle);

        app.handle_modal_key(key(KeyCode::Enter));

        assert!(app.modal.current().is_none());
        settle(&probe).await;
        assert!(probe.is_finished(), "the connect modal leaks the same way");
        assert!(!app.modal.tracks_fetch());
    }

    /// Was `dismissing_modals_aborts_both_in_flight_fetches`; there is one fetch to abort now,
    /// because there is one host.
    #[tokio::test]
    async fn dismissing_modals_aborts_an_in_flight_fetch_with_nothing_open() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        app.modal.track_fetch(handle);
        // No modal is open: cancellation is tied to TASK state, not modal state, so a handle can
        // never be stranded by the state combination the abort was gated on. `Apply` closes the
        // modal while its fetch is still in flight, which is how that state is reached.
        assert!(!app.modal.is_open());

        app.dismiss_modals();

        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "losing the session must cancel the fetch even with no modal to close"
        );
    }

    #[tokio::test]
    async fn starting_a_models_fetch_aborts_the_previous_one() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        app.modal.track_fetch(handle);

        // `local` resolves no key against the MemStore, so the replacement fetch fails offline
        // instead of touching the network.
        app.begin_model_fetch("local".to_string(), FetchSink::Models, None);

        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "re-entering the modal must not strand the previous fetch"
        );
        assert!(
            app.modal.tracks_fetch(),
            "the replacement fetch must be tracked too"
        );
    }

    #[tokio::test]
    async fn starting_a_connect_fetch_aborts_the_previous_one() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        app.modal.track_fetch(handle);

        // The mirror of `starting_a_models_fetch_aborts_the_previous_one`. Without it the four
        // abort tests only ever cover `abort_connect_fetch`, because each one assigns the field by
        // hand — `begin_fetch` could go back to a bare `tokio::spawn` with the handle dropped and
        // the whole suite would stay green, while `close_connect` would have nothing to abort and
        // the credential-bearing connection would leak exactly as before.
        //
        // `local` resolves no key against the MemStore, so the replacement fetch fails offline
        // instead of touching the network.
        app.begin_model_fetch("local".to_string(), FetchSink::Connect, None);

        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "retyping a key must not strand the previous fetch"
        );
        assert!(
            app.modal.tracks_fetch(),
            "the replacement fetch must be tracked too"
        );
    }

    #[tokio::test]
    async fn a_delivered_models_result_stops_tracking_its_task() {
        let mut app = test_app();
        let (handle, _probe) = pending_task();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.modal.track_fetch(handle);

        app.handle_models_fetched(nonce, "openai".to_string(), Ok(vec!["gpt-4o".to_string()]));

        assert!(
            !app.modal.tracks_fetch(),
            "a fetch that already delivered must not stay tracked as cancellable"
        );
    }

    #[tokio::test]
    async fn a_delivered_connect_result_stops_tracking_its_task() {
        let mut app = test_app();
        let (handle, _probe) = pending_task();
        let nonce = open(
            &mut app,
            Modal::Connect(ConnectStep::ModelList {
                rows: vec![],
                provider: "openai".to_string(),
                models: vec![],
                selected: 0,
                fetching: true,
                error: None,
                from_key: false,
            }),
        );
        app.modal.track_fetch(handle);

        app.handle_connect_models(nonce, "openai".to_string(), Ok(vec!["gpt-4o".to_string()]));

        assert!(
            !app.modal.tracks_fetch(),
            "a fetch that already delivered must not stay tracked as cancellable"
        );
    }

    #[tokio::test]
    async fn a_stale_result_leaves_the_current_fetch_tracked() {
        let mut app = test_app();
        let (handle, _probe) = pending_task();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.modal.track_fetch(handle);

        // A result from a superseded fetch must not drop the *current* fetch's cancellation
        // handle — that would strand the live, key-bearing request.
        app.handle_models_fetched(
            nonce - 1,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string()]),
        );

        assert!(app.modal.tracks_fetch());
    }

    #[tokio::test]
    async fn stepping_back_out_of_a_fetching_connect_list_aborts_the_fetch() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        app.modal.track_fetch(handle);
        open(
            &mut app,
            Modal::Connect(ConnectStep::ModelList {
                rows: vec![ProviderRow {
                    id: "openai".to_string(),
                    connected: true,
                }],
                provider: "openai".to_string(),
                models: vec![],
                selected: 0,
                fetching: true,
                error: None,
                from_key: false,
            }),
        );

        // Esc here steps back to the provider list instead of closing the modal, so
        // `close_connect` never runs — the one Esc path the abort helpers do not cover.
        app.handle_modal_key(key(KeyCode::Esc));

        assert!(
            matches!(connect_step(&app), Some(ConnectStep::ProviderList { .. })),
            "Esc from a fetching list steps back to the provider list"
        );
        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "the footer says \"Esc: cancel\"; the key-bearing request must actually stop"
        );
        assert!(!app.modal.tracks_fetch());
    }

    /// Was `closing_the_modal_returns_to_the_mode_it_was_opened_from`: there is no restore left to
    /// assert, because a modal no longer disturbs the mode it is opened over.
    #[test]
    fn closing_the_modal_leaves_the_base_mode_undisturbed() {
        let mut app = test_app();
        app.mode = Mode::Engine;
        open(
            &mut app,
            Modal::Models(models_list_step(vec!["gpt-4o".to_string()], false)),
        );
        app.handle_modal_key(key(KeyCode::Esc));
        assert!(app.modal.current().is_none());
        assert!(
            app.mode == Mode::Engine,
            "a modal never disturbs the mode it is opened over, so there is nothing to restore"
        );
    }

    #[test]
    fn losing_the_session_dismisses_an_open_models_modal() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(models_list_step(vec!["gpt-4o".to_string()], false)),
        );

        app.handle_server(ServerMessage::Error {
            code: "ws_closed".to_string(),
            message: "server closed the connection".to_string(),
        });

        assert!(
            app.modal.current().is_none(),
            "modal must not survive a sign-out"
        );
        assert!(app.mode == Mode::SignIn);
    }

    #[test]
    fn a_failed_save_rolls_back_the_staged_model() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        // A path under a non-directory can never be created, so the write always fails.
        app.settings_path = std::path::PathBuf::from("/dev/null/nope/config.json");
        app.provider_info.model = Some("stale-sentinel".to_string());
        open(
            &mut app,
            Modal::Models(ModelsStep::Manual {
                provider: "openai".to_string(),
                input: "o3".to_string(),
                error: None,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert!(
            app.settings.models.is_empty(),
            "a model that failed to save must not linger and be persisted by a later write"
        );
        assert!(app.error.is_some(), "the failure must be surfaced");
        assert_eq!(
            app.provider_info.model.as_deref(),
            Some("stale-sentinel"),
            "a failed save must not activate the model"
        );
    }

    /// The regression that decoupling help from `Mode` made possible: on master, help *was*
    /// `Mode::Help`, so assigning `self.mode` tore it down. Now nothing does implicitly, and
    /// `handle_device_result` is reached asynchronously while Ctrl-P is available from
    /// `Mode::Device`. Left open, `covers_base` would suppress the sign-in screen entirely and
    /// `handle_modal_key` would swallow every key but Ctrl-C.
    #[tokio::test]
    async fn a_failed_device_login_dismisses_an_overlay_opened_while_waiting() {
        let mut app = test_app();
        app.mode = Mode::Device;
        app.open_help();

        app.handle_device_result(
            app.device_nonce,
            Err(ApiError {
                code: "device_denied".to_string(),
                message: "denied".to_string(),
            }),
        )
        .await;

        assert!(app.mode == Mode::SignIn);
        assert!(
            app.modal.current().is_none(),
            "a modal must not outlive the screen it was opened over"
        );
    }

    /// The success half of the same asynchronous path: `enter` reaches `Mode::Connected` with the
    /// overlay still open.
    #[tokio::test]
    async fn entering_the_connected_screen_dismisses_an_overlay_opened_while_waiting() {
        let mut app = test_app();
        app.mode = Mode::Device;
        app.open_help();
        // Port 1 is never listening, so the WebSocket attempt inside `enter` is refused at once
        // instead of reaching a server a developer happens to be running.
        app.config = Config::from_url("http://127.0.0.1:1").unwrap();

        app.enter(Session {
            token: "t".to_string(),
            expires_at: 0,
            email: "a@b.c".to_string(),
            display_name: "A".to_string(),
        })
        .await;

        assert!(app.mode == Mode::Connected);
        assert!(
            app.modal.current().is_none(),
            "a modal must not outlive the screen it was opened over"
        );
    }

    /// The `ModalApply::Provider` arm of `apply_and_close_modal`: `/connect` adopts a provider as
    /// well as pinning its model, which `/models` must never do.
    #[test]
    fn connect_enter_adopts_the_provider_and_persists_its_model() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Connect(ConnectStep::ModelList {
                rows: vec![],
                provider: "openai".to_string(),
                models: vec!["gpt-4o".to_string(), "o3".to_string()],
                selected: 1,
                fetching: false,
                error: None,
                from_key: false,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert!(app.modal.current().is_none());
        assert_eq!(
            app.settings.provider.as_deref(),
            Some("openai"),
            "/connect must adopt the provider it just configured"
        );
        assert_eq!(
            app.settings.models.get("openai").map(String::as_str),
            Some("o3")
        );
        let saved = crate::settings::load_at(&app.settings_path).expect("settings were saved");
        assert_eq!(saved.provider.as_deref(), Some("openai"));
        assert_eq!(saved.models.get("openai").map(String::as_str), Some("o3"));
    }

    /// Mirrors `a_failed_save_rolls_back_the_staged_model` for the `Provider` arm: the adopted
    /// provider is staged before the write, so a failed write must put the previous one back.
    #[test]
    fn a_failed_save_rolls_back_the_adopted_provider_too() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        // A path under a non-directory can never be created, so the write always fails.
        app.settings_path = std::path::PathBuf::from("/dev/null/nope/config.json");
        app.settings.provider = Some("anthropic".to_string());
        open(
            &mut app,
            Modal::Connect(connect_model_list_step(vec!["gpt-4o".to_string()], false)),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert_eq!(
            app.settings.provider.as_deref(),
            Some("anthropic"),
            "a failed save must not leave the new provider staged"
        );
        assert!(
            app.settings.models.is_empty(),
            "a model that failed to save must not linger and be persisted by a later write"
        );
        assert!(app.error.is_some(), "the failure must be surfaced");
    }

    /// Undeclared on master and fixed here: `handle_connect_key` fired a fetch on *any* step
    /// landing on a fetching list, and the arrow-key arm carries `fetching` through — so every
    /// keypress while a list was loading spawned a duplicate request (each one carrying the API
    /// key), bumped the nonce, discarded the in-flight result, and refetched with
    /// `fetch_key = None`, dropping the key the user had just typed. Terminal key-repeat triggers
    /// it. Only a step that *begins* waiting starts a fetch now.
    ///
    /// Not a `tokio::test`: under the old behaviour this panics on `tokio::spawn` outside a
    /// runtime, which is itself proof no fetch is spawned here.
    #[test]
    fn moving_the_cursor_while_a_connect_list_loads_does_not_refetch() {
        let mut app = test_app();
        let nonce = open(
            &mut app,
            Modal::Connect(ConnectStep::ModelList {
                rows: vec![],
                provider: "openai".to_string(),
                models: vec![],
                selected: 0,
                fetching: true,
                error: None,
                from_key: true,
            }),
        );

        app.handle_modal_key(key(KeyCode::Down));
        app.handle_modal_key(key(KeyCode::Up));

        assert_eq!(
            app.modal.nonce(),
            nonce,
            "the in-flight result must survive a keypress that did not change what is awaited"
        );
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::ModelList {
                fetching: true,
                from_key: true,
                ..
            })
        ));
    }

    /// Two fetches on one modal instance, with no open/close between them: the connect list is
    /// entered, stepped back out of, and entered again. The first fetch's result must not be
    /// accepted for the second.
    #[tokio::test]
    async fn a_second_fetch_on_one_modal_does_not_accept_the_first_ones_result() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        // `local` resolves no key, so each spawned fetch fails offline instead of hitting network.
        open(
            &mut app,
            Modal::Connect(ConnectStep::ProviderList {
                rows: vec![ProviderRow {
                    id: "local".to_string(),
                    connected: true,
                }],
                selected: 0,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));
        let first = app.modal.nonce();
        app.handle_modal_key(key(KeyCode::Esc));
        app.handle_modal_key(key(KeyCode::Enter));
        let second = app.modal.nonce();

        assert_ne!(
            first, second,
            "the second fetch must not reuse the nonce the first is still carrying"
        );
        app.handle_connect_models(first, "local".to_string(), Ok(vec!["ghost".to_string()]));
        assert!(
            matches!(
                connect_step(&app),
                Some(ConnectStep::ModelList { fetching: true, models, .. }) if models.is_empty()
            ),
            "the abandoned fetch's result must not fill the list the new one is awaiting"
        );
        app.handle_connect_models(second, "local".to_string(), Ok(vec!["real".to_string()]));
        assert!(
            matches!(
                connect_step(&app),
                Some(ConnectStep::ModelList { fetching: false, models, .. })
                    if *models == vec!["real".to_string()]
            ),
            "the live fetch's result must still be accepted"
        );
    }

    #[test]
    fn opening_the_models_modal_replaces_an_open_connect_modal() {
        let mut app = test_app();
        open(
            &mut app,
            Modal::Connect(ConnectStep::ProviderList {
                rows: vec![],
                selected: 0,
            }),
        );
        app.provider_info.offline = Some(OfflineReason::NothingConfigured);
        app.enter_models();
        assert!(
            matches!(app.modal.current(), Some(Modal::Models(_))),
            "only one modal can be open at a time"
        );
    }

    #[test]
    fn opening_a_second_modal_invalidates_the_first_modals_fetch() {
        let mut app = test_app();
        let stale = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.enter_connect();
        app.handle_models_fetched(stale, "openai".to_string(), Ok(vec!["m".to_string()]));
        assert!(
            matches!(
                app.modal.current(),
                Some(Modal::Connect(ConnectStep::ProviderList { .. }))
            ),
            "a fetch from the replaced modal must not reach the new one"
        );
    }

    #[tokio::test]
    async fn models_command_error_names_the_models_command() {
        let mut app = test_app();
        app.mode = Mode::SignIn;
        let expected = app.t("status.models_not_connected").to_string();
        app.run_command("/models").await;
        assert_eq!(app.error.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn parses_models_command() {
        assert!(parse_models_command("/models"));
        assert!(parse_models_command("/models   "));
        assert!(!parse_models_command("/modelsx"));
        assert!(!parse_models_command("/model gpt-5"));
        assert!(!parse_models_command("/connect"));
    }

    #[test]
    fn handle_models_fetched_ignores_stale_nonces() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce - 1,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string()]),
        );
        assert!(matches!(
            models_step(&app),
            Some(ModelsStep::ModelList { fetching: true, models, .. }) if models.is_empty()
        ));
    }

    #[test]
    fn handle_models_fetched_pre_highlights_the_current_model() {
        let mut app = test_app();
        app.provider_info.model = Some("o3".to_string());
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string(), "o3".to_string()]),
        );
        assert!(matches!(
            models_step(&app),
            Some(ModelsStep::ModelList { fetching: false, selected: 1, models, .. })
                if models.len() == 2
        ));
    }

    #[test]
    fn handle_models_fetched_falls_back_to_the_first_row_when_the_model_is_absent() {
        let mut app = test_app();
        app.provider_info.model = Some("not-listed".to_string());
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Ok(vec!["gpt-4o".to_string(), "o3".to_string()]),
        );
        assert!(matches!(
            models_step(&app),
            Some(ModelsStep::ModelList {
                fetching: false,
                selected: 0,
                ..
            })
        ));
    }

    #[test]
    fn handle_models_fetched_falls_back_to_manual_entry_on_a_transport_error() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(FetchFailure::Fetch, "connection refused")),
        );
        let Some(ModelsStep::Manual {
            provider,
            input,
            error: Some(error),
        }) = models_step(&app)
        else {
            panic!(
                "a transport failure must keep the manual fallback, got {:?}",
                models_step(&app)
            );
        };
        assert_eq!(provider, "openai");
        assert!(input.is_empty());
        assert!(
            error.contains("connection refused"),
            "the provider's own error must survive: {error}"
        );
    }

    #[test]
    fn handle_models_fetched_routes_a_rejected_credential_to_the_credentials_step() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(
                FetchFailure::Auth,
                "HTTP status 401 Unauthorized",
            )),
        );
        let Some(ModelsStep::Credentials { provider, error }) = models_step(&app) else {
            panic!(
                "a 401 must not offer a model-id box, got {:?}",
                models_step(&app)
            );
        };
        assert_eq!(provider, "openai");
        assert!(
            error.contains("openai") && error.contains("401"),
            "the credential notice must name the provider and the cause: {error}"
        );
    }

    #[test]
    fn handle_models_fetched_routes_a_missing_key_to_the_credentials_step_verbatim() {
        let mut app = test_app();
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(FetchFailure::MissingKey, "No API key for openai")),
        );
        let Some(ModelsStep::Credentials { provider, error }) = models_step(&app) else {
            panic!(
                "a missing key must not offer a model-id box, got {:?}",
                models_step(&app)
            );
        };
        assert_eq!(provider, "openai");
        assert_eq!(
            error, "No API key for openai",
            "an already-complete sentence must not be wrapped again"
        );
    }

    /// Retrying from the credential step must re-run the fetch, not silently do nothing —
    /// `retry_models_fetch` reads the provider off the step, and `Credentials` carries one.
    #[tokio::test]
    async fn retry_re_triggers_the_fetch_from_the_credentials_step() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(ModelsStep::Credentials {
                provider: "openai".to_string(),
                error: "openai rejected the credential".to_string(),
            }),
        );
        let before = app.modal.nonce();

        app.handle_modal_key(ctrl_key(KeyCode::Char('r')));

        assert!(
            matches!(
                models_step(&app),
                Some(ModelsStep::ModelList { provider, fetching: true, .. }) if provider == "openai"
            ),
            "retry must return to a fetching list, got {:?}",
            models_step(&app)
        );
        assert_ne!(
            app.modal.nonce(),
            before,
            "the in-flight result must be invalidated"
        );
    }

    /// A blip must be recoverable in place: Ctrl+R returns the modal to a fetching list and
    /// re-runs the fetch under a fresh nonce, so the superseded in-flight result is discarded.
    #[tokio::test]
    async fn retry_re_triggers_the_fetch_from_manual_entry() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(ModelsStep::Manual {
                provider: "local".to_string(),
                input: "half-typed".to_string(),
                error: Some("boom".to_string()),
            }),
        );
        let before = app.modal.nonce();

        app.handle_modal_key(ctrl_key(KeyCode::Char('r')));

        assert!(
            matches!(
                models_step(&app),
                Some(ModelsStep::ModelList { provider, models, fetching: true, .. })
                    if provider == "local" && models.is_empty()
            ),
            "retry must return to a fetching list, got {:?}",
            models_step(&app)
        );
        assert!(
            app.modal.nonce() > before,
            "the retry must invalidate the superseded fetch"
        );
    }

    #[test]
    fn models_enter_persists_the_highlighted_model_and_rebuilds() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(ModelsStep::ModelList {
                provider: "openai".to_string(),
                models: vec!["gpt-4o".to_string(), "o3".to_string()],
                selected: 1,
                fetching: false,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert!(app.modal.current().is_none());
        assert_eq!(
            app.settings.models.get("openai").map(String::as_str),
            Some("o3")
        );
        assert!(
            app.settings.provider.is_none(),
            "/models must not activate a provider"
        );
        let saved = crate::settings::load_at(&app.settings_path).expect("settings were saved");
        assert_eq!(saved.models.get("openai").map(String::as_str), Some("o3"));
    }

    #[test]
    fn models_manual_enter_persists_the_trimmed_id() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(ModelsStep::Manual {
                provider: "openai".to_string(),
                input: "  o3-mini  ".to_string(),
                error: None,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert!(app.modal.current().is_none());
        assert_eq!(
            app.settings.models.get("openai").map(String::as_str),
            Some("o3-mini")
        );
        assert!(app.settings.provider.is_none());
        assert!(
            app.status.contains("o3-mini")
                && app.status.contains("openai")
                && app.status.contains("not verified"),
            "a blindly typed id must not be reported as verified: {}",
            app.status
        );
    }

    /// The counterpart of the test above: an id picked off the provider's own list keeps the
    /// plain, unqualified status, so the two cases stay distinguishable.
    #[test]
    fn a_picked_model_reports_the_plain_status() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(ModelsStep::ModelList {
                provider: "openai".to_string(),
                models: vec!["gpt-4o".to_string()],
                selected: 0,
                fetching: false,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert_eq!(app.status, "Model set to gpt-4o");
    }

    /// `/model <id>` is a blindly typed id by definition — nothing checked it against the
    /// provider's list — so its status must say so. This is the escape hatch the credential step
    /// now points at, and a `verified: true` here would report the flat "Model set to o3" and
    /// quietly imply the provider confirmed an id it has never seen.
    #[tokio::test]
    async fn the_model_command_reports_an_unverified_id() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        app.provider_info.id = "openai".to_string();

        app.run_command("/model o3").await;

        assert_eq!(
            app.settings.models.get("openai").map(String::as_str),
            Some("o3")
        );
        assert!(
            app.status.contains("o3")
                && app.status.contains("openai")
                && app.status.contains("not verified"),
            "/model must not claim a typed id was verified: {}",
            app.status
        );
    }

    /// A successful apply must re-derive `provider_info` from the updated settings. Asserting the
    /// resulting model directly would depend on the ambient `LIGHT_*`/API-key environment, so this
    /// plants a sentinel that only a real `rebuild_provider()` call can clear.
    #[test]
    fn models_apply_rebuilds_the_active_provider() {
        let mut app = test_app();
        let _cleanup = TempSettings(app.settings_path.clone());
        app.mode = Mode::Connected;
        app.provider_info.model = Some("stale-sentinel".to_string());
        open(
            &mut app,
            Modal::Models(ModelsStep::Manual {
                provider: "ollama".to_string(),
                input: "llama3".to_string(),
                error: None,
            }),
        );

        app.handle_modal_key(key(KeyCode::Enter));

        assert_eq!(
            app.settings.models.get("ollama").map(String::as_str),
            Some("llama3")
        );
        assert_ne!(
            app.provider_info.model.as_deref(),
            Some("stale-sentinel"),
            "apply must rebuild the active provider"
        );
    }

    #[test]
    fn models_esc_closes_without_touching_settings() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(
            &mut app,
            Modal::Models(models_list_step(vec!["gpt-4o".to_string()], false)),
        );
        app.handle_modal_key(key(KeyCode::Esc));
        assert!(app.modal.current().is_none());
        assert!(app.mode == Mode::Connected);
        assert!(app.settings.models.is_empty());
    }

    #[test]
    fn models_blank_manual_enter_stays_open_without_touching_settings() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        open(&mut app, Modal::Models(models_manual_step("   ")));
        app.handle_modal_key(key(KeyCode::Enter));
        assert!(matches!(models_step(&app), Some(ModelsStep::Manual { .. })));
        assert!(app.settings.models.is_empty());
    }

    #[test]
    fn closing_the_models_modal_invalidates_an_in_flight_fetch() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_modal_key(key(KeyCode::Esc));
        assert!(app.modal.current().is_none());
        assert_ne!(app.modal.nonce(), nonce);
    }

    #[tokio::test]
    async fn models_command_requires_a_connected_session() {
        let mut app = test_app();
        app.mode = Mode::SignIn;
        app.run_command("/models").await;
        assert!(app.modal.current().is_none());
        assert!(app.error.is_some());
    }

    #[tokio::test]
    async fn models_command_opens_the_modal_offline_when_no_provider_is_active() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        app.provider_info.offline = Some(OfflineReason::NothingConfigured);
        app.run_command("/models").await;
        assert_eq!(models_step(&app), Some(&ModelsStep::Offline));
        assert!(app.settings.models.is_empty());
    }

    #[test]
    fn handle_connect_key_blank_key_stays_on_key_entry() {
        let mut app = test_app();
        open(
            &mut app,
            Modal::Connect(ConnectStep::KeyEntry {
                rows: vec![],
                provider: "openai".to_string(),
                input: "  ".to_string(),
            }),
        );
        app.handle_modal_key(key(KeyCode::Enter));
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::KeyEntry { .. })
        ));
    }

    #[test]
    fn handle_connect_key_keyring_failure_sets_error_and_stays() {
        let mut app = test_app_with_store(Arc::new(FailingStore));
        open(
            &mut app,
            Modal::Connect(ConnectStep::KeyEntry {
                rows: vec![],
                provider: "openai".to_string(),
                input: "sk-x".to_string(),
            }),
        );
        app.handle_modal_key(key(KeyCode::Enter));
        assert!(matches!(
            connect_step(&app),
            Some(ConnectStep::KeyEntry { .. })
        ));
        assert!(app.error.is_some());
    }

    /// `store.set` returning `Ok` means the OS keyring accepted a string, not that the provider
    /// will accept it as a credential — so the status must not read as verification (#61).
    ///
    /// A `tokio::test` because `submit_key_entry` spawns the verification probe, and the body
    /// deliberately never awaits: on the current-thread test runtime nothing drives the run queue
    /// between the spawn and the end of the test, so the probe is dropped unpolled and no request
    /// is issued. Nothing on this path calls `resolve_key` either, so an exported `OPENAI_API_KEY`
    /// cannot change either assertion.
    #[tokio::test]
    async fn submitting_a_key_reports_it_as_stored_but_unverified() {
        let mut app = test_app();
        app.key_target = Some("openai".to_string());
        app.key_input = "sk-test-key".to_string();

        app.submit_key_entry();

        assert_eq!(
            app.status,
            "API key stored for openai \u{2014} not yet verified"
        );
        assert_eq!(
            app.store.get("openai").unwrap().as_deref(),
            Some("sk-test-key"),
            "the key must still be written to the store"
        );
    }

    #[tokio::test]
    async fn submitting_a_key_starts_a_probe() {
        let mut app = test_app();
        app.key_target = Some("openai".to_string());
        app.key_input = "sk-test-key".to_string();

        app.submit_key_entry();

        assert_ne!(
            app.key_probe_nonce, 0,
            "the probe generation must be bumped"
        );
        assert!(app.key_probe.is_some(), "the probe must be tracked");
    }

    /// Nothing was stored, so there is nothing to verify — and a probe here would send the key the
    /// keyring just refused to hold.
    #[tokio::test]
    async fn a_failed_keyring_write_starts_no_probe() {
        let mut app = test_app_with_store(Arc::new(FailingStore));
        app.key_target = Some("openai".to_string());
        app.key_input = "sk-test-key".to_string();

        app.submit_key_entry();

        assert!(app.error.is_some(), "the failure must be surfaced");
        assert!(app.key_probe.is_none());
        assert_eq!(app.key_probe_nonce, 0);
    }

    #[test]
    fn an_accepted_key_reports_verification() {
        let mut app = test_app();
        app.key_probe_nonce = 7;

        app.handle_key_probed(7, "openai".to_string(), Ok(()));

        assert_eq!(app.status, "openai accepted the API key");
        assert!(app.key_probe.is_none());
    }

    /// A rejected key is still the key the user asked us to hold: a 401 can come from an auth-edge
    /// outage as easily as from a bad credential, so the status carries the news and the store
    /// keeps the user's intent (#61).
    #[test]
    fn a_rejected_key_reports_rejection_and_stays_stored() {
        let store = Arc::new(MemStore::new());
        store.set("openai", "sk-test-key").unwrap();
        let mut app = test_app_with_store(store);
        app.key_probe_nonce = 7;

        app.handle_key_probed(
            7,
            "openai".to_string(),
            Err(FetchError {
                class: FetchFailure::Auth,
                message: "openai: 401".to_string(),
            }),
        );

        assert_eq!(
            app.status,
            "openai rejected the API key \u{2014} it is still stored"
        );
        assert_eq!(
            app.store.get("openai").unwrap().as_deref(),
            Some("sk-test-key"),
            "a rejected key must not be un-stored"
        );
    }

    /// Pins the shared arm: both credential classes route through `needs_credentials()`, so a probe
    /// that reports `MissingKey` cannot silently fall through to the retryable wording.
    #[test]
    fn a_missing_key_class_reports_rejection() {
        let mut app = test_app();
        app.key_probe_nonce = 7;

        app.handle_key_probed(
            7,
            "openai".to_string(),
            Err(FetchError {
                class: FetchFailure::MissingKey,
                message: "no key".to_string(),
            }),
        );

        assert_eq!(
            app.status,
            "openai rejected the API key \u{2014} it is still stored"
        );
    }

    /// A timeout, a DNS failure, or a 5xx says nothing about the key, so the status must not accuse
    /// it — and must stay distinct from the rejection wording.
    #[test]
    fn an_unreachable_provider_does_not_accuse_the_key() {
        let store = Arc::new(MemStore::new());
        store.set("openai", "sk-test-key").unwrap();
        let mut app = test_app_with_store(store);
        app.key_probe_nonce = 7;

        app.handle_key_probed(
            7,
            "openai".to_string(),
            Err(FetchError {
                class: FetchFailure::Fetch,
                message: "connection refused".to_string(),
            }),
        );

        assert_eq!(
            app.status,
            "Couldn't reach openai to verify the API key \u{2014} it is stored"
        );
        assert_ne!(
            app.status,
            "openai rejected the API key \u{2014} it is still stored"
        );
        assert_eq!(
            app.store.get("openai").unwrap().as_deref(),
            Some("sk-test-key")
        );
    }

    /// A second `/key` while the first probe is in flight must win: the older result answers a
    /// question the user has already replaced.
    #[test]
    fn a_stale_probe_result_is_discarded() {
        let mut app = test_app();
        app.key_probe_nonce = 7;
        app.status = "sentinel".to_string();

        app.handle_key_probed(6, "openai".to_string(), Ok(()));

        assert_eq!(app.status, "sentinel");
    }

    /// The abort lives inside the nonce claim so no call site can bump the generation and forget to
    /// cancel — the request carries an API key in its headers, and two probes in flight is two keys
    /// in flight.
    ///
    /// Drives `next_key_probe_nonce` directly rather than `submit_key_entry`: `settle` awaits, which
    /// would drive the run queue and poll a real probe task all the way to a live request. Claiming
    /// a nonce spawns nothing.
    #[tokio::test]
    async fn claiming_a_probe_nonce_aborts_the_previous_probe() {
        let mut app = test_app();
        let (handle, probe) = pending_task();
        app.key_probe = Some(handle);

        let nonce = app.next_key_probe_nonce();

        settle(&probe).await;
        assert!(
            probe.is_finished(),
            "a replaced probe must not be left running with the previous key"
        );
        assert_ne!(nonce, 0);
        assert!(
            app.key_probe.is_none(),
            "the aborted handle must be dropped"
        );
    }

    #[test]
    fn parses_model_commands() {
        assert_eq!(parse_model_command("/model gpt-5"), Some(Some("gpt-5")));
        assert_eq!(parse_model_command("/model"), Some(None));
        assert_eq!(parse_model_command("/model   "), Some(None));
        assert_eq!(parse_model_command("/modelx"), None);
    }

    #[test]
    fn parses_key_commands() {
        assert!(matches!(parse_key_command("/key"), Some(KeyCommand::List)));
        assert!(matches!(
            parse_key_command("/key openai"),
            Some(KeyCommand::Set(p)) if p == "openai"
        ));
        assert!(matches!(
            parse_key_command("/key openai clear"),
            Some(KeyCommand::Clear(p)) if p == "openai"
        ));
        assert!(
            matches!(parse_key_command("/key clear"), Some(KeyCommand::Set(p)) if p == "clear")
        );
        assert!(parse_key_command("/keyx").is_none());
    }

    /// Was `help_modal_opens_and_restores_the_prior_mode`.
    #[test]
    fn help_modal_opens_without_disturbing_the_base_mode() {
        let mut app = test_app();
        assert!(matches!(app.mode, Mode::SignIn));
        app.open_help();
        assert!(matches!(app.modal.current(), Some(Modal::Help)));
        // The base mode is never disturbed, so there is nothing to restore.
        assert!(matches!(app.mode, Mode::SignIn));
        app.modal.close();
        assert!(app.modal.current().is_none());
        assert!(matches!(app.mode, Mode::SignIn));
    }

    /// Was `help_modal_returns_to_the_mode_it_was_opened_from`.
    #[test]
    fn help_modal_closes_without_disturbing_the_base_mode() {
        let mut app = test_app();
        app.mode = Mode::Connected;
        app.open_help();
        assert!(matches!(app.modal.current(), Some(Modal::Help)));
        assert!(matches!(app.mode, Mode::Connected));
        app.modal.close();
        assert!(matches!(app.mode, Mode::Connected));
    }

    #[test]
    fn esc_and_ctrl_p_close_help_but_ctrl_c_quits() {
        let mut app = test_app();

        app.open_help();
        assert!(!app.handle_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.modal.current().is_none());
        assert!(matches!(app.mode, Mode::SignIn));

        app.open_help();
        assert!(!app.handle_modal_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)));
        assert!(app.modal.current().is_none());
        assert!(matches!(app.mode, Mode::SignIn));

        app.open_help();
        assert!(app.handle_modal_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }
}
