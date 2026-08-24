//! Translation catalog and locale resolution for the TUI.
//!
//! User-facing strings live in two static tables (`EN`/`ES`). Lookup falls back
//! to English, then to the key itself as a loud developer sentinel (unreachable
//! in production because `EN` completeness is test-enforced). Server errors are
//! translated by stable `code`; unknown codes surface the server message verbatim.

type Catalog = &'static [(&'static str, &'static str)];

/// The supported locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Es,
}

impl Locale {
    /// Parse a locale from a `--lang`/config/environment value, taking the primary
    /// subtag before `-`, `_`, or `.`. `en-US`, `en_US.UTF-8` → `En`; `es-419` →
    /// `Es`; anything else (including `C.UTF-8`) → `None`.
    pub fn parse(raw: &str) -> Option<Locale> {
        let primary = raw.split(['-', '_', '.']).next().unwrap_or(raw);
        match primary.to_ascii_lowercase().as_str() {
            "en" => Some(Locale::En),
            "es" => Some(Locale::Es),
            // POSIX `C`/`C.UTF-8`/`POSIX` denote the default locale (English).
            "c" | "posix" => Some(Locale::En),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Es => "es",
        }
    }
}

const EN: Catalog = &[
    ("status.not_signed_in", "Not signed in"),
    ("status.device_cancelled", "Device login cancelled"),
    ("status.unknown_command", "unknown command: {command}"),
    ("status.requesting_device_code", "Requesting device code..."),
    ("status.waiting_approval", "Waiting for browser approval..."),
    ("status.device_failed", "Device login failed"),
    ("status.email_required", "Email is required"),
    ("status.code_required", "TOTP code is required"),
    ("status.signing_in", "Signing in..."),
    ("status.creating_account", "Creating account..."),
    (
        "status.scan_confirm",
        "Scan the QR / enter the secret, then confirm",
    ),
    ("status.confirming", "Confirming..."),
    ("status.connecting", "Connecting..."),
    ("status.connected_as", "Connected as {email}"),
    ("status.connect_failed", "could not connect: {error}"),
    ("status.ws_failed", "Signed in, but the WebSocket failed"),
    ("status.signed_out", "Signed out"),
    ("status.ready", "Ready: {name} <{email}>"),
    ("status.pong", "Pong {nonce}"),
    ("status.ping", "Ping {nonce}"),
    ("status.disconnected", "Disconnected: {reason}"),
    ("status.lang_set", "Language set to {lang}"),
    ("status.lang_invalid", "Unknown language; use 'en' or 'es'"),
    ("logout_done", "Logged out."),
    ("screen.sign_in", "Sign in"),
    ("screen.create_account", "Create account"),
    ("screen.complete_registration", "Complete registration"),
    ("screen.device_login", "Device login"),
    ("field.email", "Email"),
    ("field.code", "Code"),
    ("field.name", "Name"),
    (
        "hint.sign_in",
        "Passwordless: enter the 6-digit code from your authenticator app",
    ),
    (
        "hint.register",
        "Registering sends a TOTP secret to pair with your authenticator app",
    ),
    ("hint.account", "Account: {email}"),
    (
        "hint.open_url",
        "Open this URL (or scan it as a QR code) in your authenticator app:",
    ),
    ("hint.manual_secret", "Manual secret: "),
    (
        "hint.device_line1",
        "A browser window should have opened. Sign in — or create an",
    ),
    (
        "hint.device_line2",
        "account — there, then click Authorize.",
    ),
    ("hint.device_visit", "If the browser did not open, visit:"),
    (
        "hint.device_code_filled",
        "This device code is already filled in on the page:",
    ),
    (
        "hint.device_waiting",
        "Waiting for approval in your browser...",
    ),
    ("hint.not_signed_in", "not signed in"),
    (
        "hint.no_messages",
        "No messages yet — press 'p' to ping the server",
    ),
    ("hint.help", "Ctrl-P: help"),
    ("hint.help_close", "Ctrl-P / Esc: close"),
    ("hint.device_cancel", "Esc: cancel device login"),
    ("title.activity", "Activity"),
    (
        "info.connected",
        "{name} <{email}>  ·  provider: {provider}  ·  pongs: {pongs}",
    ),
    (
        "provider.offline.nothing",
        "No provider configured — set ANTHROPIC_API_KEY (or another provider's key) or LIGHT_OLLAMA=1",
    ),
    (
        "provider.offline.missing_key",
        "Provider '{selector}' selected but {key} is not set — falling back to offline",
    ),
    (
        "provider.offline.base_url",
        "{var} was rejected — falling back to offline",
    ),
    (
        "provider.store.unavailable",
        "Could not read the stored key for {provider}: {error}",
    ),
    (
        "provider.store.unavailable_all",
        "Could not read the credential store: {error}",
    ),
    (
        "provider.offline.store_unavailable",
        "Falling back to the offline provider: the credential store could not be read, so stored keys were unavailable. Unlock it and restart, or set ANTHROPIC_API_KEY (or another provider's key) in the environment.",
    ),
    ("status.ask_empty", "Usage: /ask <prompt>"),
    (
        "status.ask_not_connected",
        "/ask is available after you sign in",
    ),
    ("error.invalid_credentials", "Invalid email or code"),
    (
        "error.email_taken",
        "An account with that email already exists",
    ),
    ("error.invalid_email", "Invalid email address"),
    ("error.invalid_totp_code", "Invalid TOTP code"),
    ("error.invalid_challenge", "Invalid or expired challenge"),
    ("error.invalid_session", "Invalid or expired session"),
    ("error.invalid_grant", "Invalid device code"),
    ("error.expired_token", "Device authorization expired"),
    ("error.storage_error", "Storage error"),
    ("error.internal_error", "Internal server error"),
    ("error.invalid_json", "Invalid request"),
    ("error.network", "Could not reach the server"),
    ("error.decode", "Unexpected response from the server"),
    ("error.bad_message", "Could not parse server message"),
    ("error.ws_closed", "Connection closed"),
    (
        "error.turn_in_progress",
        "a turn is already running; prompt ignored",
    ),
    (
        "error.no_provider_configured",
        "No provider configured — set an API key or LIGHT_OLLAMA=1",
    ),
    ("engine.plan_proposed", "plan proposed: {summary}"),
    ("engine.plan_approved", "plan approved"),
    ("engine.plan_rejected", "plan rejected"),
    ("engine.file_edit", "wrote {path} ({bytes} bytes)"),
    ("engine.command_run", "ran {command} (exit {code})"),
    ("engine.approval_needed", "approval needed: {detail}"),
    ("engine.token_usage", "tokens: {input} in / {output} out"),
    ("engine.turn_complete", "turn complete"),
    ("engine.turn_ended", "turn ended"),
    (
        "engine.plan_prompt",
        "Plan: {summary}\n{steps} step(s), {paths} write path(s), {commands} command(s)",
    ),
    (
        "engine.reason_outside_scope",
        "outside the approved scope: {what}",
    ),
    ("engine.reason_sensitive", "sensitive path: {path}"),
    ("engine.approve_keys", "[a] approve  [d] deny"),
    ("engine.dropped_events", "dropped {count} engine event(s)"),
    ("title.engine", "Engine"),
    ("status.engine_started", "Engine session started"),
    ("provider.reason.ollama_env", "LIGHT_OLLAMA"),
    ("provider.reason.selector_env", "LIGHT_REMOTE_PROVIDER"),
    ("provider.reason.stored", "stored preference"),
    ("provider.reason.key_precedence", "key precedence"),
    ("provider.reason.offline", "offline"),
    ("provider.key.env", "env"),
    ("provider.key.keyring", "keyring"),
    ("provider.key.none", "none"),
    ("provider.key.unavailable", "unavailable"),
    ("key.list", "keys: {list}"),
    ("status.model_set", "Model set to {model}"),
    (
        "status.model_set_unverified",
        "Model set to {model} \u{2014} not verified against {provider}",
    ),
    (
        "status.settings_save_failed",
        "Couldn't save settings: {error}",
    ),
    ("status.model_empty", "Usage: /model <id>"),
    (
        "status.model_unsupported",
        "The active provider has no model to set",
    ),
    ("status.key_set", "API key saved for {provider}"),
    ("status.key_cleared", "API key cleared for {provider}"),
    (
        "status.key_failed",
        "Could not save the API key for {provider}: {error}",
    ),
    (
        "status.key_unsupported",
        "{provider} does not take an API key",
    ),
    (
        "status.key_enter",
        "Enter API key for {provider} (input hidden)",
    ),
    ("status.key_empty", "No key entered; nothing saved"),
    ("field.key", "API key"),
    ("hint.key", "Enter: save · Esc: cancel"),
    ("title.help", "Help"),
    ("help.section.global", "Global"),
    ("help.global.help", "Ctrl-P  this help"),
    ("help.global.quit", "Ctrl-C  quit"),
    ("help.section.forms", "Forms"),
    ("help.forms.navigate", "Tab / ↑↓  next field"),
    ("help.forms.submit", "Enter  submit"),
    ("help.forms.command", "/  command"),
    ("help.forms.back", "Esc  back / close"),
    ("help.section.connected", "Connected"),
    ("help.connected.ping", "p  ping server"),
    ("help.connected.signout", "o  sign out"),
    ("help.connected.engine", "e  engine mode"),
    ("help.connected.quit", "q  quit"),
    ("help.section.engine", "Engine"),
    ("help.engine.send", "Enter  send prompt"),
    ("help.engine.back", "Esc  back"),
    (
        "help.engine.approve",
        "a / d  approve / deny (while a decision is pending)",
    ),
    ("help.section.commands", "Commands"),
    ("help.commands.ask", "/ask <prompt>  completion"),
    ("help.commands.connect", "/connect  connect a provider"),
    ("help.commands.model", "/model <id>  set model"),
    ("help.commands.models", "/models  pick a model"),
    ("help.commands.key", "/key [provider] [clear]  API key"),
    (
        "help.commands.auth",
        "/auth/login  /auth/logout  sign in / out",
    ),
    ("help.commands.lang", "/lang <en|es>  language"),
    (
        "status.connect_not_connected",
        "/connect is available after you sign in",
    ),
    (
        "status.models_not_connected",
        "/models is available after you sign in",
    ),
    ("connect.title", "Connect a provider"),
    ("connect.key_heading", "API Key"),
    ("connect.connected", "connected"),
    ("connect.store_unavailable_row", "key store unavailable"),
    ("connect.models_heading", "Models: {provider}"),
    ("connect.fetching", "Fetching models..."),
    ("connect.fetch_error", "Couldn't fetch models: {error}"),
    (
        "connect.fetch_panicked",
        "the fetch task failed unexpectedly",
    ),
    ("connect.no_models", "No models reported"),
    (
        "connect.footer_list",
        "Esc: close · Enter: select · \u{2191}/\u{2193}: navigate",
    ),
    ("connect.footer_key", "Enter: submit · Esc: back"),
    ("connect.footer_models", "Enter: select · Esc: back"),
    ("connect.footer_fetching", "Esc: cancel"),
    ("connect.no_key", "No API key for {provider}"),
    (
        "connect.store_unavailable",
        "the credential store for {provider} could not be read: {error}",
    ),
    ("models.title", "Select a model"),
    ("models.offline", "Use /connect to connect a provider first"),
    (
        "models.manual_unverified",
        "Type a model id \u{2014} it won't be checked against {provider}",
    ),
    (
        "models.auth_rejected",
        "{provider} rejected the credential: {error}",
    ),
    (
        "models.credentials_hint",
        "Typing a model id can't fix this.",
    ),
    (
        "models.credentials_remedy",
        "Use /connect, /key {provider}, or /model <id>",
    ),
    (
        "models.store_remedy",
        "Unlock the credential store and retry, or set {provider}'s API key in the environment",
    ),
    (
        "models.footer_list",
        "Enter: select · Esc: close · \u{2191}/\u{2193}: navigate",
    ),
    (
        "models.footer_manual",
        "Enter: save unverified · Ctrl+R: retry · Esc: close",
    ),
    ("models.footer_retry", "Ctrl+R: retry · Esc: close"),
    ("models.footer_offline", "Esc: close"),
];

const ES: Catalog = &[
    ("status.not_signed_in", "Sin iniciar sesión"),
    (
        "status.device_cancelled",
        "Inicio de sesión del dispositivo cancelado",
    ),
    ("status.unknown_command", "comando desconocido: {command}"),
    (
        "status.requesting_device_code",
        "Solicitando código del dispositivo...",
    ),
    (
        "status.waiting_approval",
        "Esperando aprobación en el navegador...",
    ),
    (
        "status.device_failed",
        "Falló el inicio de sesión del dispositivo",
    ),
    (
        "status.email_required",
        "El correo electrónico es obligatorio",
    ),
    ("status.code_required", "El código TOTP es obligatorio"),
    ("status.signing_in", "Iniciando sesión..."),
    ("status.creating_account", "Creando cuenta..."),
    (
        "status.scan_confirm",
        "Escanea el QR o introduce el secreto y confirma",
    ),
    ("status.confirming", "Confirmando..."),
    ("status.connecting", "Conectando..."),
    ("status.connected_as", "Conectado como {email}"),
    ("status.connect_failed", "no se pudo conectar: {error}"),
    (
        "status.ws_failed",
        "Sesión iniciada, pero el WebSocket falló",
    ),
    ("status.signed_out", "Sesión cerrada"),
    ("status.ready", "Listo: {name} <{email}>"),
    ("status.pong", "Pong {nonce}"),
    ("status.ping", "Ping {nonce}"),
    ("status.disconnected", "Desconectado: {reason}"),
    ("status.lang_set", "Idioma cambiado a {lang}"),
    ("status.lang_invalid", "Idioma desconocido; usa 'en' o 'es'"),
    ("logout_done", "Sesión cerrada."),
    ("screen.sign_in", "Iniciar sesión"),
    ("screen.create_account", "Crear cuenta"),
    ("screen.complete_registration", "Completar registro"),
    ("screen.device_login", "Inicio de sesión del dispositivo"),
    ("field.email", "Correo electrónico"),
    ("field.code", "Código"),
    ("field.name", "Nombre"),
    (
        "hint.sign_in",
        "Sin contraseña: introduce el código de 6 dígitos de tu aplicación de autenticación",
    ),
    (
        "hint.register",
        "Al registrarte se envía un secreto TOTP para vincular tu aplicación de autenticación",
    ),
    ("hint.account", "Cuenta: {email}"),
    (
        "hint.open_url",
        "Abre esta URL (o escanéala como código QR) en tu aplicación de autenticación:",
    ),
    ("hint.manual_secret", "Secreto manual: "),
    (
        "hint.device_line1",
        "Se debería haber abierto una ventana del navegador. Inicia sesión — o crea una",
    ),
    (
        "hint.device_line2",
        "cuenta — allí, y luego haz clic en Autorizar.",
    ),
    ("hint.device_visit", "Si el navegador no se abrió, visita:"),
    (
        "hint.device_code_filled",
        "Este código de dispositivo ya está rellenado en la página:",
    ),
    (
        "hint.device_waiting",
        "Esperando aprobación en tu navegador...",
    ),
    ("hint.not_signed_in", "sin iniciar sesión"),
    (
        "hint.no_messages",
        "Aún no hay mensajes: pulsa 'p' para hacer ping al servidor",
    ),
    ("hint.help", "Ctrl-P: ayuda"),
    ("hint.help_close", "Ctrl-P / Esc: cerrar"),
    (
        "hint.device_cancel",
        "Esc: cancelar inicio de sesión del dispositivo",
    ),
    ("title.activity", "Actividad"),
    (
        "info.connected",
        "{name} <{email}>  ·  proveedor: {provider}  ·  pongs: {pongs}",
    ),
    (
        "provider.offline.nothing",
        "No hay proveedor configurado — define ANTHROPIC_API_KEY (o la clave de otro proveedor) o LIGHT_OLLAMA=1",
    ),
    (
        "provider.offline.missing_key",
        "Proveedor '{selector}' seleccionado pero {key} no está definida — usando modo sin conexión",
    ),
    (
        "provider.offline.base_url",
        "{var} fue rechazada — usando modo sin conexión",
    ),
    (
        "provider.store.unavailable",
        "No se pudo leer la clave guardada de {provider}: {error}",
    ),
    (
        "provider.store.unavailable_all",
        "No se pudo leer el almac\u{e9}n de credenciales: {error}",
    ),
    (
        "provider.offline.store_unavailable",
        "Usando el proveedor sin conexi\u{f3}n: no se pudo leer el almac\u{e9}n de credenciales, as\u{ed} que las claves guardadas no estaban disponibles. Desbloqu\u{e9}alo y reinicia, o define ANTHROPIC_API_KEY (o la clave de otro proveedor) en el entorno.",
    ),
    ("status.ask_empty", "Uso: /ask <indicación>"),
    (
        "status.ask_not_connected",
        "/ask está disponible después de iniciar sesión",
    ),
    ("error.invalid_credentials", "Correo o código no válidos"),
    (
        "error.email_taken",
        "Ya existe una cuenta con ese correo electrónico",
    ),
    (
        "error.invalid_email",
        "Dirección de correo electrónico no válida",
    ),
    ("error.invalid_totp_code", "Código TOTP no válido"),
    ("error.invalid_challenge", "Desafío no válido o caducado"),
    ("error.invalid_session", "Sesión no válida o caducada"),
    ("error.invalid_grant", "Código de dispositivo no válido"),
    (
        "error.expired_token",
        "La autorización del dispositivo ha caducado",
    ),
    ("error.storage_error", "Error de almacenamiento"),
    ("error.internal_error", "Error interno del servidor"),
    ("error.invalid_json", "Solicitud no válida"),
    ("error.network", "No se pudo contactar con el servidor"),
    ("error.decode", "Respuesta inesperada del servidor"),
    (
        "error.bad_message",
        "No se pudo analizar el mensaje del servidor",
    ),
    ("error.ws_closed", "Conexión cerrada"),
    (
        "error.turn_in_progress",
        "ya hay un turno en curso; indicación ignorada",
    ),
    (
        "error.no_provider_configured",
        "No hay proveedor configurado — define una clave de API o LIGHT_OLLAMA=1",
    ),
    ("engine.plan_proposed", "plan propuesto: {summary}"),
    ("engine.plan_approved", "plan aprobado"),
    ("engine.plan_rejected", "plan rechazado"),
    ("engine.file_edit", "escrito {path} ({bytes} bytes)"),
    ("engine.command_run", "ejecutado {command} (salida {code})"),
    ("engine.approval_needed", "se requiere aprobación: {detail}"),
    (
        "engine.token_usage",
        "tokens: {input} entrada / {output} salida",
    ),
    ("engine.turn_complete", "turno completado"),
    ("engine.turn_ended", "turno finalizado"),
    (
        "engine.plan_prompt",
        "Plan: {summary}\n{steps} paso(s), {paths} ruta(s) de escritura, {commands} comando(s)",
    ),
    (
        "engine.reason_outside_scope",
        "fuera del alcance aprobado: {what}",
    ),
    ("engine.reason_sensitive", "ruta sensible: {path}"),
    ("engine.approve_keys", "[a] aprobar  [d] denegar"),
    (
        "engine.dropped_events",
        "se omitieron {count} evento(s) del motor",
    ),
    ("title.engine", "Motor"),
    ("status.engine_started", "Sesión del motor iniciada"),
    ("provider.reason.ollama_env", "LIGHT_OLLAMA"),
    ("provider.reason.selector_env", "LIGHT_REMOTE_PROVIDER"),
    ("provider.reason.stored", "preferencia guardada"),
    ("provider.reason.key_precedence", "precedencia de claves"),
    ("provider.reason.offline", "sin conexión"),
    ("provider.key.env", "entorno"),
    ("provider.key.keyring", "llavero"),
    ("provider.key.none", "ninguna"),
    ("provider.key.unavailable", "no disponible"),
    ("key.list", "claves: {list}"),
    ("status.model_set", "Modelo cambiado a {model}"),
    (
        "status.model_set_unverified",
        "Modelo cambiado a {model} \u{2014} no verificado con {provider}",
    ),
    (
        "status.settings_save_failed",
        "No se pudieron guardar los ajustes: {error}",
    ),
    ("status.model_empty", "Uso: /model <id>"),
    (
        "status.model_unsupported",
        "El proveedor activo no tiene modelo que configurar",
    ),
    ("status.key_set", "Clave de API guardada para {provider}"),
    (
        "status.key_cleared",
        "Clave de API eliminada para {provider}",
    ),
    (
        "status.key_failed",
        "No se pudo guardar la clave de API para {provider}: {error}",
    ),
    (
        "status.key_unsupported",
        "{provider} no usa una clave de API",
    ),
    (
        "status.key_enter",
        "Introduce la clave de API para {provider} (entrada oculta)",
    ),
    (
        "status.key_empty",
        "No se introdujo ninguna clave; no se guardó nada",
    ),
    ("field.key", "Clave de API"),
    ("hint.key", "Enter: guardar · Esc: cancelar"),
    ("title.help", "Ayuda"),
    ("help.section.global", "Global"),
    ("help.global.help", "Ctrl-P  esta ayuda"),
    ("help.global.quit", "Ctrl-C  salir"),
    ("help.section.forms", "Formularios"),
    ("help.forms.navigate", "Tab / ↑↓  siguiente campo"),
    ("help.forms.submit", "Enter  enviar"),
    ("help.forms.command", "/  comando"),
    ("help.forms.back", "Esc  atrás / cerrar"),
    ("help.section.connected", "Conectado"),
    ("help.connected.ping", "p  ping al servidor"),
    ("help.connected.signout", "o  cerrar sesión"),
    ("help.connected.engine", "e  modo motor"),
    ("help.connected.quit", "q  salir"),
    ("help.section.engine", "Motor"),
    ("help.engine.send", "Enter  enviar indicación"),
    ("help.engine.back", "Esc  volver"),
    (
        "help.engine.approve",
        "a / d  aprobar / denegar (solo con una decisión pendiente)",
    ),
    ("help.section.commands", "Comandos"),
    ("help.commands.ask", "/ask <indicación>  completar"),
    ("help.commands.connect", "/connect  conectar un proveedor"),
    ("help.commands.model", "/model <id>  configurar modelo"),
    ("help.commands.models", "/models  elegir un modelo"),
    (
        "help.commands.key",
        "/key [proveedor] [clear]  clave de API",
    ),
    (
        "help.commands.auth",
        "/auth/login  /auth/logout  iniciar / cerrar sesión",
    ),
    ("help.commands.lang", "/lang <en|es>  idioma"),
    (
        "status.connect_not_connected",
        "/connect está disponible después de iniciar sesión",
    ),
    (
        "status.models_not_connected",
        "/models está disponible después de iniciar sesión",
    ),
    ("connect.title", "Conectar un proveedor"),
    ("connect.key_heading", "Clave de API"),
    ("connect.connected", "conectado"),
    (
        "connect.store_unavailable_row",
        "almac\u{e9}n de claves no disponible",
    ),
    ("connect.models_heading", "Modelos: {provider}"),
    ("connect.fetching", "Obteniendo modelos..."),
    (
        "connect.fetch_error",
        "No se pudieron obtener los modelos: {error}",
    ),
    (
        "connect.fetch_panicked",
        "la tarea de obtenci\u{f3}n fall\u{f3} inesperadamente",
    ),
    ("connect.no_models", "No se informaron modelos"),
    (
        "connect.footer_list",
        "Esc: cerrar · Enter: elegir · \u{2191}/\u{2193}: navegar",
    ),
    ("connect.footer_key", "Enter: enviar · Esc: volver"),
    ("connect.footer_models", "Enter: elegir · Esc: volver"),
    ("connect.footer_fetching", "Esc: cancelar"),
    ("connect.no_key", "No hay clave de API para {provider}"),
    (
        "connect.store_unavailable",
        "no se pudo leer el almac\u{e9}n de credenciales de {provider}: {error}",
    ),
    ("models.title", "Seleccionar un modelo"),
    (
        "models.offline",
        "Usa /connect para conectar un proveedor primero",
    ),
    (
        "models.manual_unverified",
        "Escribe un id de modelo \u{2014} no se verificará con {provider}",
    ),
    (
        "models.auth_rejected",
        "{provider} rechazó la credencial: {error}",
    ),
    (
        "models.credentials_hint",
        "Escribir un id de modelo no soluciona esto.",
    ),
    (
        "models.credentials_remedy",
        "Usa /connect, /key {provider}, o /model <id>",
    ),
    (
        "models.store_remedy",
        "Desbloquea el almac\u{e9}n de credenciales y reintenta, o define la clave de API de {provider} en el entorno",
    ),
    (
        "models.footer_list",
        "Enter: elegir · Esc: cerrar · \u{2191}/\u{2193}: navegar",
    ),
    // "sin verificar" is dropped deliberately: with it the line is 63 columns against a
    // 58-column inner width and the footer is rendered without `Wrap`, so it was hard-truncated
    // and ES users silently lost "Esc: cerrar". The prompt line above already says the id is not
    // verified.
    (
        "models.footer_manual",
        "Enter: guardar · Ctrl+R: reintentar · Esc: cerrar",
    ),
    ("models.footer_retry", "Ctrl+R: reintentar · Esc: cerrar"),
    ("models.footer_offline", "Esc: cerrar"),
];

fn lookup(catalog: Catalog, key: &str) -> Option<&'static str> {
    catalog.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Translate `key` for `locale`, falling back to English, then to `key`.
pub fn t(locale: Locale, key: &str) -> &str {
    if let Some(found) = match locale {
        Locale::Es => lookup(ES, key).or_else(|| lookup(EN, key)),
        Locale::En => lookup(EN, key),
    } {
        return found;
    }
    key
}

/// Translate `key` and substitute `{param}` placeholders from `params`.
pub fn t_with(locale: Locale, key: &str, params: &[(&str, &str)]) -> String {
    let mut out = t(locale, key).to_string();
    for (name, value) in params {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Translate a stable server/client error `code`, or `None` when the code is
/// unrecognized (callers then surface the server message verbatim).
pub fn error_message(locale: Locale, code: &str) -> Option<&'static str> {
    let key = format!("error.{code}");
    lookup(EN, &key)?;
    match locale {
        Locale::Es => lookup(ES, &key).or_else(|| lookup(EN, &key)),
        Locale::En => lookup(EN, &key),
    }
}

/// Pure resolution: `--lang` → saved config → `LC_ALL` → `LANG` → English.
pub fn resolve_locale(
    cli: Option<&str>,
    saved: Option<&str>,
    lang_env: Option<&str>,
    lc_all_env: Option<&str>,
) -> Locale {
    if let Some(raw) = cli
        && let Some(locale) = Locale::parse(raw)
    {
        return locale;
    }
    if let Some(raw) = saved
        && let Some(locale) = Locale::parse(raw)
    {
        return locale;
    }
    if let Some(raw) = lc_all_env
        && let Some(locale) = Locale::parse(raw)
    {
        return locale;
    }
    if let Some(raw) = lang_env
        && let Some(locale) = Locale::parse(raw)
    {
        return locale;
    }
    Locale::En
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(catalog: Catalog) -> Vec<&'static str> {
        let mut v: Vec<_> = catalog.iter().map(|(k, _)| *k).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn es_mirrors_en_exactly() {
        assert_eq!(keys(ES), keys(EN), "ES must define exactly the EN key set");
    }

    /// Footers are pinned to one row and rendered without `Wrap`, so anything past the popup's
    /// inner width is hard-truncated and the tail of the line is silently lost. This bit ES first
    /// — `models.footer_manual` was 63 columns and cost ES users "Esc: cerrar" — and was invisible
    /// because every render assertion runs in EN.
    #[test]
    fn every_footer_fits_the_popup_in_both_locales() {
        // `draw_popup` draws a 60-column box with a one-column border on each side.
        const INNER_WIDTH: usize = 58;
        for (catalog, name) in [(EN, "EN"), (ES, "ES")] {
            for (key, value) in catalog {
                if !key.contains(".footer") {
                    continue;
                }
                let columns = value.chars().count();
                assert!(
                    columns <= INNER_WIDTH,
                    "{name} {key} is {columns} columns; it is truncated at {INNER_WIDTH}: {value}"
                );
            }
        }
    }

    /// The connect modal's provider rows share the footers' 60-column popup (58 inner), but
    /// `every_footer_fits_the_popup_in_both_locales` gates `*.footer` keys only. `anthropic` is
    /// the longest id in `PROVIDER_NAMES`, and the row is drawn as `"> {id} ({suffix})"`.
    #[test]
    fn the_unavailable_row_suffix_fits_the_popup_in_both_locales() {
        const INNER_WIDTH: usize = 58;
        for (locale, name) in [(Locale::En, "EN"), (Locale::Es, "ES")] {
            let row = format!(
                "> anthropic ({})",
                t(locale, "connect.store_unavailable_row")
            );
            let columns = row.chars().count();
            assert!(
                columns <= INNER_WIDTH,
                "{name} row is {columns} columns: {row}"
            );
        }
    }

    #[test]
    fn parses_locales() {
        assert_eq!(Locale::parse("en"), Some(Locale::En));
        assert_eq!(Locale::parse("EN"), Some(Locale::En));
        assert_eq!(Locale::parse("en-US"), Some(Locale::En));
        assert_eq!(Locale::parse("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::parse("es"), Some(Locale::Es));
        assert_eq!(Locale::parse("es-419"), Some(Locale::Es));
        assert_eq!(Locale::parse("fr"), None);
        assert_eq!(Locale::parse("C"), Some(Locale::En));
        assert_eq!(Locale::parse("C.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::parse("POSIX"), Some(Locale::En));
        assert_eq!(Locale::parse(""), None);
    }

    #[test]
    fn resolves_with_precedence() {
        // --lang wins over everything.
        assert_eq!(
            resolve_locale(Some("es"), Some("en"), Some("en"), Some("en")),
            Locale::Es
        );
        // saved config beats environment.
        assert_eq!(
            resolve_locale(None, Some("es"), Some("en"), Some("en")),
            Locale::Es
        );
        // LANG is used when nothing higher is set.
        assert_eq!(resolve_locale(None, None, Some("es"), None), Locale::Es);
        // LC_ALL takes precedence over LANG.
        assert_eq!(
            resolve_locale(None, None, Some("en"), Some("es")),
            Locale::Es
        );
        // LC_ALL=C denotes the default locale and overrides a non-English LANG.
        assert_eq!(
            resolve_locale(None, None, Some("es"), Some("C.UTF-8")),
            Locale::En
        );
        assert_eq!(resolve_locale(None, None, None, None), Locale::En);
        // invalid values fall through.
        assert_eq!(resolve_locale(Some("fr"), None, None, None), Locale::En);
    }

    #[test]
    fn translates_and_falls_back() {
        assert_ne!(
            t(Locale::Es, "screen.sign_in"),
            t(Locale::En, "screen.sign_in")
        );
        assert_eq!(t(Locale::En, "screen.sign_in"), "Sign in");
        assert_eq!(t(Locale::Es, "screen.sign_in"), "Iniciar sesión");
        // Missing key falls back to English, then to the key.
        assert_eq!(t(Locale::Es, "definitely.missing"), "definitely.missing");
    }

    #[test]
    fn interpolates_params() {
        assert_eq!(
            t_with(Locale::En, "status.connected_as", &[("email", "a@b.c")]),
            "Connected as a@b.c"
        );
        assert_eq!(
            t_with(Locale::Es, "status.connected_as", &[("email", "a@b.c")]),
            "Conectado como a@b.c"
        );
    }

    #[test]
    fn translates_known_error_codes_only() {
        assert_eq!(
            error_message(Locale::Es, "invalid_credentials"),
            Some("Correo o código no válidos")
        );
        assert_eq!(
            error_message(Locale::En, "invalid_credentials"),
            Some("Invalid email or code")
        );
        assert_eq!(error_message(Locale::Es, "no_such_code"), None);
        assert_eq!(error_message(Locale::En, "no_such_code"), None);
    }
}
