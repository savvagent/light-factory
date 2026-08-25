//! light-factory TUI entry point.

mod api;
mod app;
mod browser;
mod config;
mod modal;
mod provider;
mod selection;
mod session;
mod settings;
mod text;
mod ws;

use std::sync::Arc;

use clap::Parser;
use light_factory_tui::credentials::{CredentialStore, KeyringStore};
use light_factory_tui::i18n::{self, Locale};

/// Terminal UI client for the light-factory agentic coding platform.
#[derive(Parser)]
#[command(name = "light-factory", version, about, long_about = None)]
struct Cli {
    /// Base URL of the light-factory server.
    #[arg(long, env = "LIGHT_API_URL", default_value = "http://localhost:8080")]
    url: String,

    /// Email to pre-fill on the sign-in form.
    #[arg(long)]
    email: Option<String>,

    /// Language for the interface (en, es). Overrides the saved preference and
    /// environment, and persists for future runs.
    #[arg(long)]
    lang: Option<String>,

    /// Clear the saved session and exit.
    #[arg(long)]
    logout: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut settings = settings::SettingsHandle::load();
    let locale = resolve_locale(&cli, &mut settings);

    if cli.logout {
        session::Session::clear()?;
        println!("{}", i18n::t(locale, "logout_done"));
        return Ok(());
    }

    let config = config::Config::from_url(&cli.url)?.with_lang(locale);
    let store: Arc<dyn CredentialStore> = Arc::new(KeyringStore);
    let (provider, info) = selection::rebuild(&settings.settings, store.as_ref());
    app::run(config, provider, info, store, settings, cli.email).await
}

/// Resolve the locale from `--lang` → saved config → `LC_ALL` → `LANG` → English,
/// persisting the explicit `--lang` choice.
fn resolve_locale(cli: &Cli, handle: &mut settings::SettingsHandle) -> Locale {
    let lc_all = std::env::var("LC_ALL").ok();
    let lang_env = std::env::var("LANG").ok();
    let saved = (!handle.settings.lang.is_empty()).then_some(handle.settings.lang.as_str());

    let locale = i18n::resolve_locale(
        cli.lang.as_deref(),
        saved,
        lang_env.as_deref(),
        lc_all.as_deref(),
    );

    if let Some(raw) = cli.lang.as_deref()
        && let Some(parsed) = Locale::parse(raw)
    {
        handle.settings.lang = parsed.as_str().to_string();
        let _ = settings::save_at(&handle.path, &handle.settings);
    }

    locale
}
