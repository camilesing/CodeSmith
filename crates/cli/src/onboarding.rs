//! First-run onboarding as plain command-line questions.
//!
//! Replaces the former TUI wizard: a bare `codesmith` on a machine without
//! the `~/.codesmith/.onboarded` marker answers language → provider →
//! endpoint → API key → directory trust on stdin, writes the same durable
//! state the wizard wrote, and then launches the TUI as usual.
//!
//! Known limitations:
//! - The API key is echoed in cleartext, matching `codesmith auth set` and
//!   `login` prompting; the workspace has no masked-input dependency.
//! - Moonshot Kimi OAuth is not offered here. Pick "moonshot" with an API
//!   key, or link OAuth later via the TUI `/provider` picker.
//! - Non-terminal stdin never triggers the wizard, and only the bare
//!   `codesmith` invocation runs it — first runs of subcommands fall
//!   through to the TUI's credential/trust gates.
//! - The config file is written through a raw TOML merge (not
//!   `ConfigStore::save`) so unknown fields such as `[projects]` survive.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use codesmith_agent_runtime::onboarding::{is_onboarded, mark_onboarded, needs_trust};
use codesmith_agent_runtime::workspace_trust::apply_workspace_trust;
use codesmith_config::ProviderKind;
use codesmith_config::{
    custom_endpoint_provider_default, ensure_config_file_at, heavy_model_for_provider,
    provider_is_self_hosted, write_config_file_private,
};
use codesmith_secrets::Secrets;

use crate::main_root::Cli;
use crate::{PROVIDER_LIST, provider_env_value, provider_slot, write_provider_api_key_to_keyring};

/// Language options for the first question. Mirrors the TUI wizard's
/// `LANGUAGE_OPTIONS`: `auto` defers to `LC_ALL` / `LANG` and is still the
/// persisted tag.
const LANGUAGE_OPTIONS: [&str; 6] = ["auto", "en", "zh-Hans", "zh-Hant", "hi", "es-419"];

/// Run the CLI onboarding wizard when first-run onboarding is due.
///
/// Gate: the user did not opt out (`--skip-onboarding`), the
/// `~/.codesmith/.onboarded` marker is missing, and stdin is a terminal.
/// On completion `store` is reloaded so later runtime resolution sees the
/// wizard's writes.
pub(crate) fn maybe_run(store: &mut codesmith_config::ConfigStore, cli: &Cli) -> Result<()> {
    if cli.skip_onboarding || is_onboarded() || !io::stdin().is_terminal() {
        return Ok(());
    }

    run_wizard(store, cli)?;

    // The wizard wrote config.toml behind the store's back (raw TOML merge);
    // reload so `resolve_runtime_for_dispatch` and the TUI delegation see
    // the same file.
    *store = codesmith_config::ConfigStore::load(cli.config.clone())?;
    Ok(())
}

fn run_wizard(store: &mut codesmith_config::ConfigStore, cli: &Cli) -> Result<()> {
    let workspace = cli.workspace.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    println!("Welcome to CodeSmith!");
    println!(
        "This one-time setup asks a few questions; answers are saved to {}.",
        store.path().display()
    );
    println!();

    let locale = pick_language()?;
    let provider: ProviderKind = match cli.provider {
        Some(arg) => arg.into(),
        None => pick_provider()?,
    };

    let endpoint = if custom_endpoint_provider_default(provider).is_some() {
        match &cli.base_url {
            Some(url) => {
                require_valid_endpoint(url)?;
                println!("Using --base-url {url}");
                Some(url.clone())
            }
            None => prompt_endpoint(provider)?,
        }
    } else {
        None
    };

    let api_key = resolve_api_key(store, cli, provider)?;

    // Mirror the TUI wizard's trust screen: declining exits before anything
    // is written (the TUI's 'n' sends Op::Shutdown).
    let trust_needed = !cli.yolo && needs_trust(&workspace);
    if trust_needed && !prompt_trust(&workspace)? {
        bail!(
            "Workspace not trusted. CodeSmith needs a trust decision for {} before \
             starting; re-run `codesmith` and answer 'y', or start from a directory \
             you trust.",
            workspace.display()
        );
    }

    let secrets = Secrets::auto_detect();
    persist_config(
        store,
        cli,
        &workspace,
        provider,
        endpoint.as_deref(),
        api_key.as_deref(),
        trust_needed,
        &secrets,
    )?;

    codesmith_agent_runtime::settings_file::write_settings_locale(locale)
        .context("failed to save the language choice")?;
    // The marker is the commit point: everything above may half-fail and a
    // re-run simply asks again.
    mark_onboarded().context("failed to record onboarding completion")?;

    println!("Setup complete. Starting CodeSmith…");
    println!("  - Check credentials any time: codesmith auth status");
    println!("  - Switch provider/model later: /provider and /model in the TUI");
    println!();
    Ok(())
}

// ── Questions ─────────────────────────────────────────────────────────

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    let read = io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed to read answer from stdin")?;
    if read == 0 {
        // EOF (Ctrl-D / closed stdin): fail loud instead of feeding every
        // retry loop an eternal stream of empty answers.
        bail!(
            "stdin closed before onboarding finished; run `codesmith` again in a terminal to complete setup"
        );
    }
    Ok(line.trim().to_string())
}

fn pick_language() -> Result<&'static str> {
    println!("Language:");
    for (idx, tag) in LANGUAGE_OPTIONS.iter().enumerate() {
        println!("  {}) {tag}", idx + 1);
    }
    loop {
        let answer = prompt_line("Choice [1]: ")?;
        if answer.is_empty() {
            return Ok("auto");
        }
        let parsed = answer.parse::<usize>();
        if let Some(tag) = parsed.ok().and_then(|idx| LANGUAGE_OPTIONS.get(idx - 1)) {
            return Ok(tag);
        }
        println!("Enter a number between 1 and {}.", LANGUAGE_OPTIONS.len());
    }
}

fn pick_provider() -> Result<ProviderKind> {
    println!("Provider:");
    for (idx, provider) in PROVIDER_LIST.iter().enumerate() {
        let suffix = if provider_is_self_hosted(*provider) {
            "  (no API key needed)"
        } else {
            ""
        };
        println!("  {:2}) {}{suffix}", idx + 1, provider_display(*provider));
    }
    loop {
        let answer = prompt_line("Choice [1]: ")?;
        if answer.is_empty() {
            return Ok(ProviderKind::Deepseek);
        }
        let parsed = answer.parse::<usize>();
        if let Some(provider) = parsed.ok().and_then(|idx| PROVIDER_LIST.get(idx - 1)) {
            return Ok(*provider);
        }
        println!("Enter a number between 1 and {}.", PROVIDER_LIST.len());
    }
}

fn provider_display(provider: ProviderKind) -> String {
    match codesmith_agent_runtime::config_types::ApiProvider::parse(provider.as_str()) {
        Some(parsed) => parsed.display_name().to_string(),
        None => provider.as_str().to_string(),
    }
}

fn prompt_endpoint(provider: ProviderKind) -> Result<Option<String>> {
    let default =
        custom_endpoint_provider_default(provider).expect("caller checked the endpoint gate");
    println!();
    println!(
        "Endpoint for {} (Enter = {default})",
        provider_display(provider)
    );
    loop {
        let answer = prompt_line("Base URL: ")?;
        if answer.is_empty() {
            return Ok(None);
        }
        match validate_endpoint_url_for_onboarding(&answer) {
            InputValidation::Accept { warning } => {
                if let Some(warning) = warning {
                    println!("Warning: {warning}");
                }
                return Ok(Some(answer));
            }
            InputValidation::Reject(message) => println!("{message}"),
        }
    }
}

fn require_valid_endpoint(url: &str) -> Result<()> {
    match validate_endpoint_url_for_onboarding(url) {
        InputValidation::Accept { warning } => {
            if let Some(warning) = warning {
                println!("Warning: {warning}");
            }
            Ok(())
        }
        InputValidation::Reject(message) => bail!("--base-url {url}: {message}"),
    }
}

/// Returns `Some(key)` when one was entered or supplied via `--api-key`, and
/// `None` when an existing credential (env var or saved key) already covers
/// the provider — the same inputs `has_api_key_for` checks in the TUI.
fn resolve_api_key(
    store: &codesmith_config::ConfigStore,
    cli: &Cli,
    provider: ProviderKind,
) -> Result<Option<String>> {
    if let Some(key) = &cli.api_key {
        match validate_api_key_for_onboarding(key) {
            InputValidation::Accept { warning } => {
                if let Some(warning) = warning {
                    println!("Warning: {warning}");
                }
            }
            InputValidation::Reject(message) => bail!("--api-key: {message}"),
        }
        return Ok(Some(key.clone()));
    }
    if provider_is_self_hosted(provider) {
        println!(
            "{} serves locally; no API key needed.",
            provider_display(provider)
        );
        return Ok(None);
    }
    if let Some((var, _)) = provider_env_value(provider) {
        println!("Using the {var} environment variable; no key entry needed.");
        return Ok(None);
    }
    let secrets = Secrets::auto_detect();
    if secrets
        .get(provider_slot(provider))
        .ok()
        .flatten()
        .is_some()
    {
        println!(
            "Using the API key saved in the {} keyring slot.",
            provider_slot(provider)
        );
        return Ok(None);
    }
    if crate::provider_config_api_key(store, provider).is_some() {
        println!("Using the API key saved in {}.", store.path().display());
        return Ok(None);
    }

    println!();
    println!(
        "API key for {} (echoed in cleartext):",
        provider_display(provider)
    );
    loop {
        let answer = prompt_line("API key: ")?;
        match validate_api_key_for_onboarding(&answer) {
            InputValidation::Accept { warning } => {
                if let Some(warning) = warning {
                    println!("Warning: {warning}");
                }
                return Ok(Some(answer));
            }
            InputValidation::Reject(message) => println!("{message}"),
        }
    }
}

fn prompt_trust(workspace: &Path) -> Result<bool> {
    println!();
    println!("Trust this workspace and load its project config and .env?");
    println!("  {}", workspace.display());
    loop {
        let answer = prompt_line("Trust? (y/n): ")?;
        match answer.as_str() {
            "y" | "Y" => return Ok(true),
            "n" | "N" => return Ok(false),
            _ => println!("Please answer y or n."),
        }
    }
}

// ── Persistence ───────────────────────────────────────────────────────

/// Write the wizard's answers into the config file (template first, then a
/// single raw-TOML merge that preserves unknown fields) and the keyring.
/// The locale write and the `.onboarded` marker stay with the caller so a
/// failure anywhere above leaves a re-runnable state.
#[allow(clippy::too_many_arguments)]
fn persist_config(
    store: &codesmith_config::ConfigStore,
    cli: &Cli,
    workspace: &Path,
    provider: ProviderKind,
    endpoint: Option<&str>,
    api_key: Option<&str>,
    trusted: bool,
    secrets: &Secrets,
) -> Result<()> {
    let config_path = store.path();
    ensure_config_file_at(config_path)?;

    let mut doc = if config_path.exists() {
        let raw = std::fs::read_to_string(config_path)
            .with_context(|| format!("failed to read config at {}", config_path.display()))?;
        toml::from_str::<toml::Value>(&raw)
            .with_context(|| format!("failed to parse config at {}", config_path.display()))?
    } else {
        bail!(
            "config template was not created at {}",
            config_path.display()
        );
    };

    let root = doc
        .as_table_mut()
        .context("Config root must be a TOML table.")?;
    root.insert(
        "provider".to_string(),
        toml::Value::String(provider.as_str().to_string()),
    );
    if let Some(key) = api_key {
        root.insert(
            "auth_mode".to_string(),
            toml::Value::String("api_key".to_string()),
        );
        if provider == ProviderKind::Deepseek {
            // DeepSeek keeps a root `api_key` mirror for legacy readers.
            root.insert("api_key".to_string(), toml::Value::String(key.to_string()));
        }
    }

    let providers = root
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .context("`providers` must be a table.")?;
    let field = provider_toml_field(provider);
    let table = providers
        .entry(field.to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .context("Provider entry must be a table.")?;

    if let Some(url) = endpoint {
        table.insert("base_url".to_string(), toml::Value::String(url.to_string()));
    }
    if let Some(key) = api_key {
        table.insert("api_key".to_string(), toml::Value::String(key.to_string()));
    }
    // Pin a per-provider model only when the file cannot resolve one on its
    // own: DeepSeek reads the template's `default_text_model`, every other
    // provider would otherwise boot on a DeepSeek model id (#1714
    // pass-through).
    let has_model = table
        .get("model")
        .and_then(toml::Value::as_str)
        .is_some_and(|model| !model.trim().is_empty());
    if !has_model && provider != ProviderKind::Deepseek {
        let model = cli
            .model_flag
            .clone()
            .unwrap_or_else(|| heavy_model_for_provider(provider).to_string());
        table.insert("model".to_string(), toml::Value::String(model));
    }

    if trusted {
        apply_workspace_trust(&mut doc, workspace)?;
    }

    let serialized = toml::to_string_pretty(&doc).context("failed to serialize config")?;
    write_config_file_private(config_path, &serialized)
        .with_context(|| format!("failed to write config at {}", config_path.display()))?;

    if let Some(key) = api_key
        && !write_provider_api_key_to_keyring(secrets, provider, key)
    {
        println!(
            "Note: could not reach the OS keyring; the key is saved in {} only.",
            config_path.display()
        );
    }
    Ok(())
}

/// `[providers.<field>]` table name for a provider — the serde field names of
/// `codesmith_config::ProvidersToml` (snake_case), not the kebab-case
/// `provider_slot` used for keyring entries.
fn provider_toml_field(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Deepseek => "deepseek",
        ProviderKind::NvidiaNim => "nvidia_nim",
        ProviderKind::Openai => "openai",
        ProviderKind::Atlascloud => "atlascloud",
        ProviderKind::WanjieArk => "wanjie_ark",
        ProviderKind::Volcengine => "volcengine",
        ProviderKind::Openrouter => "openrouter",
        ProviderKind::XiaomiMimo => "xiaomi_mimo",
        ProviderKind::Novita => "novita",
        ProviderKind::Fireworks => "fireworks",
        ProviderKind::Siliconflow => "siliconflow",
        ProviderKind::Moonshot => "moonshot",
        ProviderKind::Sglang => "sglang",
        ProviderKind::Vllm => "vllm",
        ProviderKind::Ollama => "ollama",
        ProviderKind::Anthropic => "anthropic",
    }
}

// ── Input validation (mirrors the TUI onboarding rules) ───────────────

/// Result of inspecting a text field entered during onboarding (an API key, a
/// custom endpoint URL).
///
/// `Accept` always lets the user proceed; the optional `warning` is printed
/// as a non-blocking note (short keys, plain-http endpoints, …). `Reject`
/// blocks until the user fixes the input.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InputValidation {
    Accept { warning: Option<String> },
    Reject(String),
}

/// Validate an API key entered during onboarding. Whitespace-only or
/// whitespace-containing keys are rejected; short or hyphen-less keys
/// are accepted with a warning so unusual provider key formats still
/// work.
fn validate_api_key_for_onboarding(api_key: &str) -> InputValidation {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return InputValidation::Reject("API key cannot be empty.".to_string());
    }
    if trimmed.contains(char::is_whitespace) {
        return InputValidation::Reject(
            "API key appears malformed (contains whitespace).".to_string(),
        );
    }
    if trimmed.len() < 16 {
        return InputValidation::Accept {
            warning: Some(
                "API key looks short. Double-check it, but unusual formats are allowed."
                    .to_string(),
            ),
        };
    }
    if !trimmed.contains('-') {
        return InputValidation::Accept {
            warning: Some(
                "API key format looks unusual. Check that the full key was copied.".to_string(),
            ),
        };
    }
    InputValidation::Accept { warning: None }
}

/// Validate a custom endpoint URL entered during onboarding.
///
/// Requires an absolute `http://` / `https://` URL with a host. Plain `http`
/// to anything but a local address is accepted **with a warning**: a trusted
/// LAN endpoint is a documented setup for this project, but the API key then
/// travels in the clear, and a proxy URL is exactly the case where the
/// credential would be sent somewhere the user did not intend.
fn validate_endpoint_url_for_onboarding(url: &str) -> InputValidation {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return InputValidation::Reject("Endpoint URL cannot be empty.".to_string());
    }
    if trimmed.contains(char::is_whitespace) {
        return InputValidation::Reject("Endpoint URL contains whitespace.".to_string());
    }
    let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    else {
        return InputValidation::Reject(
            "Endpoint URL must start with https:// or http://.".to_string(),
        );
    };
    if rest.is_empty() || rest.starts_with('/') {
        return InputValidation::Reject("Endpoint URL is missing a host.".to_string());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let is_local = authority.starts_with("localhost")
        || authority.starts_with("127.")
        || authority.starts_with("[::1]")
        || authority.ends_with(".local");
    if trimmed.starts_with("http://") && !is_local {
        return InputValidation::Accept {
            warning: Some(
                "Plain http sends your API key in the clear — use https unless this is a \
                 trusted LAN endpoint."
                    .to_string(),
            ),
        };
    }
    InputValidation::Accept { warning: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_empty_or_whitespace_keys() {
        assert!(matches!(
            validate_api_key_for_onboarding(""),
            InputValidation::Reject(_)
        ));
        assert!(matches!(
            validate_api_key_for_onboarding("   "),
            InputValidation::Reject(_)
        ));
        assert!(matches!(
            validate_api_key_for_onboarding("sk live abc"),
            InputValidation::Reject(_)
        ));
    }

    #[test]
    fn validate_warns_on_short_or_no_hyphen_keys_but_accepts() {
        match validate_api_key_for_onboarding("abc123") {
            InputValidation::Accept { warning: Some(_) } => {}
            other => panic!("expected accept-with-warning, got {other:?}"),
        }
        assert_eq!(
            validate_api_key_for_onboarding("sk-1234567890abcdef"),
            InputValidation::Accept { warning: None }
        );
    }

    #[test]
    fn endpoint_url_rejects_empty_relative_or_scheme_less_input() {
        for input in ["", "   ", "/v1", "api.openai.com/v1", "ftp://host/v1"] {
            assert!(
                matches!(
                    validate_endpoint_url_for_onboarding(input),
                    InputValidation::Reject(_)
                ),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn endpoint_url_accepts_https_and_local_http_and_warns_on_remote_http() {
        assert_eq!(
            validate_endpoint_url_for_onboarding("https://proxy.example.com/v1"),
            InputValidation::Accept { warning: None }
        );
        assert_eq!(
            validate_endpoint_url_for_onboarding("http://127.0.0.1:8080/v1"),
            InputValidation::Accept { warning: None }
        );
        match validate_endpoint_url_for_onboarding("http://gateway.lan:8000/v1") {
            InputValidation::Accept {
                warning: Some(message),
            } => assert!(message.contains("clear"), "got {message:?}"),
            other => panic!("expected accept-with-warning, got {other:?}"),
        }
    }

    #[test]
    fn provider_toml_field_matches_providers_table_names() {
        // Spot-check the snake_case fields that differ from as_str().
        assert_eq!(provider_toml_field(ProviderKind::NvidiaNim), "nvidia_nim");
        assert_eq!(provider_toml_field(ProviderKind::XiaomiMimo), "xiaomi_mimo");
        assert_eq!(provider_toml_field(ProviderKind::Deepseek), "deepseek");
    }

    #[test]
    fn persist_config_writes_provider_key_model_and_trust() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let store = codesmith_config::ConfigStore::load(Some(config_path.clone()))
            .expect("load empty store");
        let cli = test_cli();

        persist_config(
            &store,
            &cli,
            dir.path(),
            ProviderKind::Moonshot,
            None,
            Some("sk-test-1234567890abcdef"),
            true,
            &crate::no_keyring_secrets(),
        )
        .expect("persist");

        let raw = std::fs::read_to_string(&config_path).expect("read config");
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");
        assert_eq!(
            doc.get("provider").and_then(toml::Value::as_str),
            Some("moonshot")
        );
        assert_eq!(
            doc.get("auth_mode").and_then(toml::Value::as_str),
            Some("api_key")
        );
        let moonshot = doc
            .get("providers")
            .and_then(|p| p.get("moonshot"))
            .expect("providers.moonshot");
        assert_eq!(
            moonshot.get("api_key").and_then(toml::Value::as_str),
            Some("sk-test-1234567890abcdef")
        );
        assert_eq!(
            moonshot.get("model").and_then(toml::Value::as_str),
            Some(heavy_model_for_provider(ProviderKind::Moonshot))
        );
        // `apply_workspace_trust` files the workspace under its canonical
        // path (symlinks resolved — /var → /private/var on macOS).
        let canonical = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        assert!(
            doc.get("projects")
                .and_then(|p| p.get(canonical.to_string_lossy().as_ref()))
                .and_then(|p| p.get("trust_level"))
                .and_then(toml::Value::as_str)
                .is_some(),
            "trust entry missing for {}",
            canonical.display()
        );

        // DeepSeek keeps no provider model pin — the root template model
        // stays authoritative — but does mirror the root key.
        let deepseek_dir = tempfile::tempdir().expect("tempdir");
        let deepseek_path = deepseek_dir.path().join("config.toml");
        let deepseek_store = codesmith_config::ConfigStore::load(Some(deepseek_path.clone()))
            .expect("load empty store");
        persist_config(
            &deepseek_store,
            &cli,
            deepseek_dir.path(),
            ProviderKind::Deepseek,
            None,
            Some("sk-test-1234567890abcdef"),
            false,
            &crate::no_keyring_secrets(),
        )
        .expect("persist deepseek");
        let raw = std::fs::read_to_string(&deepseek_path).expect("read config");
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");
        assert!(
            doc.get("providers")
                .and_then(|p| p.get("deepseek"))
                .and_then(|p| p.get("model"))
                .is_none()
        );
        assert_eq!(
            doc.get("api_key").and_then(toml::Value::as_str),
            Some("sk-test-1234567890abcdef")
        );
    }

    fn test_cli() -> Cli {
        use clap::Parser;
        Cli::parse_from(["codesmith"])
    }
}
