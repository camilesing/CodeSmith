mod metrics;
mod update;

pub(crate) use codesmith_agent_runtime::auto_reasoning;

use main_root::Cli;

mod acp_server;
mod agent_memory;
mod app_state;
mod approval_grants;
mod artifacts;
mod audit;
mod auto_mode;
mod automation_manager;
mod background_task;
mod bundled_skills;
mod capabilities;
mod child_env;
pub mod clipboard;
mod command_safety;
mod compaction;
mod config;
mod core;
mod cost_status;
mod cycle_manager;
mod dependencies;
mod doctor_llm;
mod eval;
mod execpolicy;
mod extension_state;
mod features;
mod handoff;
mod hooks;
mod index;
mod interactive;
mod knowledge;
mod llm_client;
mod localization;
mod logging;
mod lsp;
mod main_root;
mod mcp;
mod mcp_server;
mod media;
mod memory;
mod memory_consolidate;
mod mod_ops;
mod mod_state;
mod models;
mod network_policy;
mod notifications;
mod onboarding;
mod prefix_cache;
mod presets;
mod pricing;
mod project_context;
mod prompts;
mod purge;
pub mod repl;
mod retry_status;
pub mod rlm;
mod runtime_api;
mod runtime_threads;
mod sandbox;
mod schema_migration;
mod seam_manager;
mod session_manager;
mod settings;
mod shell_dispatcher;
mod skill_state;
mod skills;
mod slop_ledger;
mod task_manager;
#[cfg(test)]
mod test_support;
mod tool_summary;
mod tools;
mod utils;
mod vision;
mod working_set;
mod workspace_trust;

use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::builder::StyledStr;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Args, CommandFactory, Subcommand, ValueEnum};
use codesmith_agent::ModelRegistry;
use codesmith_app_server::{
    AppServerOptions, run as run_app_server, run_stdio as run_app_server_stdio,
};
use codesmith_config::{ConfigStore, ProviderKind};
use codesmith_execpolicy::{AskForApproval, ExecPolicyContext, ExecPolicyEngine};
use codesmith_mcp::{McpServerDefinition, run_stdio_server};
use codesmith_secrets::Secrets;
use codesmith_state::{StateStore, ThreadListFilters};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ProviderArg {
    Deepseek,
    NvidiaNim,
    Openai,
    Atlascloud,
    WanjieArk,
    Volcengine,
    Openrouter,
    XiaomiMimo,
    Novita,
    Fireworks,
    Siliconflow,
    Moonshot,
    Sglang,
    Vllm,
    Ollama,
    Anthropic,
}

impl From<ProviderArg> for ProviderKind {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Deepseek => ProviderKind::Deepseek,
            ProviderArg::NvidiaNim => ProviderKind::NvidiaNim,
            ProviderArg::Openai => ProviderKind::Openai,
            ProviderArg::Atlascloud => ProviderKind::Atlascloud,
            ProviderArg::WanjieArk => ProviderKind::WanjieArk,
            ProviderArg::Volcengine => ProviderKind::Volcengine,
            ProviderArg::Openrouter => ProviderKind::Openrouter,
            ProviderArg::XiaomiMimo => ProviderKind::XiaomiMimo,
            ProviderArg::Novita => ProviderKind::Novita,
            ProviderArg::Fireworks => ProviderKind::Fireworks,
            ProviderArg::Siliconflow => ProviderKind::Siliconflow,
            ProviderArg::Moonshot => ProviderKind::Moonshot,
            ProviderArg::Sglang => ProviderKind::Sglang,
            ProviderArg::Vllm => ProviderKind::Vllm,
            ProviderArg::Ollama => ProviderKind::Ollama,
            ProviderArg::Anthropic => ProviderKind::Anthropic,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct UpdateArgs {
    /// Update to the latest beta release instead of the latest stable release.
    #[arg(long)]
    beta: bool,
    /// Only check the latest release; do not download or replace binaries.
    #[arg(long)]
    check: bool,
    /// Proxy URL to use for update HTTP requests.
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct MetricsArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
    /// Restrict to events newer than this duration (e.g. 7d, 24h, 30m, now-2h).
    #[arg(long, value_name = "DURATION")]
    since: Option<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct LoginArgs {
    #[arg(long, value_enum, hide = true)]
    provider: Option<ProviderArg>,
    #[arg(long)]
    api_key: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AuthArgs {
    #[command(subcommand)]
    pub(crate) command: AuthCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum AuthCommand {
    Status,
    Set {
        #[arg(long, value_enum)]
        provider: ProviderArg,
        /// Inline value (discouraged — appears in shell history).
        #[arg(long)]
        api_key: Option<String>,
        /// Read the key from stdin instead of prompting.
        #[arg(long = "api-key-stdin", default_value_t = false)]
        api_key_stdin: bool,
    },
    Get {
        #[arg(long, value_enum)]
        provider: ProviderArg,
    },
    Clear {
        #[arg(long, value_enum)]
        provider: ProviderArg,
    },
    List,
    /// Advanced: migrate config-file keys into a platform credential store.
    #[command(hide = true)]
    Migrate {
        /// Don't actually write anything; print what would change.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub(crate) command: ConfigCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ConfigCommand {
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
    List,
    Path,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ModelArgs {
    #[command(subcommand)]
    pub(crate) command: ModelCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ModelCommand {
    List {
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
    },
    Resolve {
        model: Option<String>,
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
    },
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ThreadArgs {
    #[command(subcommand)]
    pub(crate) command: ThreadCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ThreadCommand {
    List {
        #[arg(long, default_value_t = false)]
        all: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    Read {
        thread_id: String,
    },
    Resume {
        thread_id: String,
    },
    Fork {
        thread_id: String,
    },
    Archive {
        thread_id: String,
    },
    Unarchive {
        thread_id: String,
    },
    SetName {
        thread_id: String,
        name: String,
    },
    ClearName {
        thread_id: String,
    },
}

#[derive(Debug, Args)]
struct SandboxArgs {
    #[command(subcommand)]
    command: SandboxCommand,
}

#[derive(Debug, Subcommand)]
enum SandboxCommand {
    Check {
        command: String,
        #[arg(long, value_enum, default_value_t = ApprovalModeArg::OnRequest)]
        ask: ApprovalModeArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ApprovalModeArg {
    UnlessTrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl From<ApprovalModeArg> for AskForApproval {
    fn from(value: ApprovalModeArg) -> Self {
        match value {
            ApprovalModeArg::UnlessTrusted => AskForApproval::UnlessTrusted,
            ApprovalModeArg::OnFailure => AskForApproval::OnFailure,
            ApprovalModeArg::OnRequest => AskForApproval::OnRequest,
            ApprovalModeArg::Never => AskForApproval::Never,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AppServerArgs {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8787)]
    port: u16,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long = "auth-token")]
    auth_token: Option<String>,
    #[arg(long, default_value_t = false)]
    insecure_no_auth: bool,
    #[arg(long = "cors-origin")]
    cors_origin: Vec<String>,
    #[arg(long, default_value_t = false)]
    stdio: bool,
}

const MCP_SERVER_DEFINITIONS_KEY: &str = "mcp.server_definitions";

/// Static snapshot of the container quick start. `docs/DOCKER.md` is the
/// canonical copy: changing the image tag, volume layout, or env var there
/// means updating this string and `docs/CLI.md` / `docs/CLI_cn.md` in the
/// same change.
const DOCKER_USAGE: &str = "\
Run CodeSmith in the published container image:

  docker volume create codesmith-home

  docker run --rm -it \\
    -e CODESMITH_API_KEY=\"$CODESMITH_API_KEY\" \\
    -v codesmith-home:/home/codesmith/.codesmith \\
    -v \"$PWD:/workspace\" \\
    -w /workspace \\
    ghcr.io/camilesing/codesmith:latest

Pinned tags, local image builds, volume ownership, and non-interactive
pipelines: https://github.com/camilesing/CodeSmith/blob/main/docs/DOCKER.md
";

/// True when argv holds exactly one free (non-option) word — the shape in
/// which clap reports `InvalidSubcommand` for a mistyped subcommand name.
/// Free words cannot reach the model: the prompt is delivered only through
/// the `-p`/`--prompt` flag, so a lone bare word can only be a typo, and
/// routing it through `unknown_subcommand_error` gives the user the `-p`
/// hint instead of clap's raw usage error.
pub(crate) fn is_single_bare_word<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> bool {
    let mut free = 0usize;
    for arg in argv.iter().skip(1) {
        let arg = arg.as_ref();
        if arg == "--" {
            // Everything after the terminator is prompt text by intent.
            return false;
        }
        let Some(text) = arg.to_str() else {
            return false;
        };
        if text.starts_with('-') {
            continue;
        }
        free += 1;
        if free > 1 {
            return false;
        }
    }
    free == 1
}

/// clap's own `unrecognized subcommand` usage error (exit code 2) carrying the
/// two ways out: `-p` to send the word as a prompt, `--help` for the command list.
///
/// Built through the context path rather than `Command::error`, because a raw
/// message is rendered as-is and would drop the `tip:` lines; the context
/// formatter is what draws them.
fn unknown_subcommand_error(word: &str) -> clap::Error {
    let mut cmd = Cli::command();
    let usage = cmd.render_usage();
    let mut err = clap::Error::new(ErrorKind::InvalidSubcommand).with_cmd(&cmd);
    err.insert(
        ContextKind::InvalidSubcommand,
        ContextValue::String(word.to_string()),
    );
    err.insert(ContextKind::Usage, ContextValue::StyledStr(usage));
    err.insert(
        ContextKind::Suggested,
        ContextValue::StyledStrs(vec![
            StyledStr::from(format!(
                "to send this as a prompt, run: codesmith -p {word}"
            )),
            StyledStr::from("run `codesmith --help` to see all commands"),
        ]),
    );
    err
}

pub fn run_cli() -> std::process::ExitCode {
    match main_root::real_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // Use the full anyhow chain so callers see the underlying
            // cause (e.g. the actual TOML parse error with line/column)
            // instead of just the top-level context message. The bare
            // `{err}` Display impl drops the chain — see #767, where
            // users hit "failed to parse config at <path>" with no
            // hint that the real error was a stray BOM or unbalanced
            // quote a few lines down.
            eprintln!("error: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_login_command(store: &mut ConfigStore, args: LoginArgs) -> Result<()> {
    run_login_command_with_secrets(store, args, &Secrets::auto_detect())
}

fn run_login_command_with_secrets(
    store: &mut ConfigStore,
    args: LoginArgs,
    secrets: &Secrets,
) -> Result<()> {
    let provider: ProviderKind = args.provider.unwrap_or(ProviderArg::Deepseek).into();
    store.config.provider = provider;

    let api_key = match args.api_key {
        Some(v) => v,
        None => read_api_key_from_stdin()?,
    };
    write_provider_api_key_to_config(store, provider, &api_key);
    let keyring_saved = write_provider_api_key_to_keyring(secrets, provider, &api_key);
    store.save()?;
    let destination = if keyring_saved {
        format!("{} and {}", store.path().display(), secrets.backend_name())
    } else {
        store.path().display().to_string()
    };
    if provider == ProviderKind::Deepseek {
        println!("logged in using API key mode (deepseek); saved key to {destination}");
    } else {
        println!(
            "logged in using API key mode ({}); saved key to {destination}",
            provider.as_str(),
        );
    }
    Ok(())
}

pub(crate) fn run_logout_command(store: &mut ConfigStore) -> Result<()> {
    run_logout_command_with_secrets(store, &Secrets::auto_detect())
}

fn run_logout_command_with_secrets(store: &mut ConfigStore, secrets: &Secrets) -> Result<()> {
    let active_provider = store.config.provider;
    store.config.api_key = None;
    for provider in PROVIDER_LIST {
        clear_provider_api_key_from_config(store, provider);
    }
    clear_provider_api_key_from_keyring(secrets, active_provider);
    store.config.auth_mode = None;
    store.save()?;
    println!("logged out");
    Ok(())
}

/// Map [`ProviderKind`] to the canonical provider credential slot.
fn provider_slot(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Deepseek => "deepseek",
        ProviderKind::NvidiaNim => "nvidia-nim",
        ProviderKind::Openai => "openai",
        ProviderKind::Atlascloud => "atlascloud",
        ProviderKind::WanjieArk => "wanjie-ark",
        ProviderKind::Volcengine => "volcengine",
        ProviderKind::Openrouter => "openrouter",
        ProviderKind::XiaomiMimo => "xiaomi-mimo",
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

/// Provider order used by the `auth list` and `auth status` outputs.
const PROVIDER_LIST: [ProviderKind; 16] = [
    ProviderKind::Deepseek,
    ProviderKind::NvidiaNim,
    ProviderKind::Openai,
    ProviderKind::Atlascloud,
    ProviderKind::WanjieArk,
    ProviderKind::Volcengine,
    ProviderKind::Openrouter,
    ProviderKind::XiaomiMimo,
    ProviderKind::Novita,
    ProviderKind::Fireworks,
    ProviderKind::Siliconflow,
    ProviderKind::Moonshot,
    ProviderKind::Sglang,
    ProviderKind::Vllm,
    ProviderKind::Ollama,
    ProviderKind::Anthropic,
];

#[cfg(test)]
fn no_keyring_secrets() -> Secrets {
    Secrets::new(std::sync::Arc::new(
        codesmith_secrets::InMemoryKeyringStore::new(),
    ))
}

fn write_provider_api_key_to_keyring(
    secrets: &Secrets,
    provider: ProviderKind,
    api_key: &str,
) -> bool {
    secrets.set(provider_slot(provider), api_key).is_ok()
}
/// Move plaintext keys from config.toml into the configured secret store.
/// Hidden in v0.8.8 because the normal setup path is config/env only.
fn run_auth_migrate(store: &mut ConfigStore, secrets: &Secrets, dry_run: bool) -> Result<()> {
    let mut migrated: Vec<(ProviderKind, &'static str)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for provider in PROVIDER_LIST {
        let slot = provider_slot(provider);
        let from_provider_block = store
            .config
            .providers
            .for_provider(provider)
            .api_key
            .clone()
            .filter(|v| !v.trim().is_empty());
        let from_root = (provider == ProviderKind::Deepseek)
            .then(|| store.config.api_key.clone())
            .flatten()
            .filter(|v| !v.trim().is_empty());
        let value = from_provider_block.or(from_root);
        let Some(value) = value else { continue };

        if let Ok(Some(existing)) = secrets.get(slot)
            && existing == value
        {
            // Already migrated; safe to strip the file slot.
        } else if dry_run {
            migrated.push((provider, slot));
            continue;
        } else if let Err(err) = secrets.set(slot, &value) {
            warnings.push(format!(
                "skipped {slot}: failed to write to secret store: {err}"
            ));
            continue;
        }
        if !dry_run {
            store.config.providers.for_provider_mut(provider).api_key = None;
            if provider == ProviderKind::Deepseek {
                store.config.api_key = None;
            }
        }
        migrated.push((provider, slot));
    }

    if !dry_run && !migrated.is_empty() {
        store
            .save()
            .context("failed to write updated config.toml")?;
    }

    println!("secret store backend: {}", secrets.backend_name());
    if migrated.is_empty() {
        println!("nothing to migrate (config.toml has no plaintext api_key entries)");
    } else {
        println!(
            "{} {} provider key(s):",
            if dry_run { "would migrate" } else { "migrated" },
            migrated.len()
        );
        for (_, slot) in &migrated {
            println!("  - {slot}");
        }
        if !dry_run {
            println!(
                "config.toml at {} no longer contains api_key entries for migrated providers.",
                store.path().display()
            );
        }
    }
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}
fn write_provider_api_key_to_config(
    store: &mut ConfigStore,
    provider: ProviderKind,
    api_key: &str,
) {
    store.config.provider = provider;
    store.config.auth_mode = Some("api_key".to_string());
    store.config.providers.for_provider_mut(provider).api_key = Some(api_key.to_string());
    if provider == ProviderKind::Deepseek {
        store.config.api_key = Some(api_key.to_string());
        if store.config.default_text_model.is_none() {
            store.config.default_text_model = Some(
                store
                    .config
                    .providers
                    .deepseek
                    .model
                    .clone()
                    .unwrap_or_else(|| "deepseek-v4-pro".to_string()),
            );
        }
    }
}

fn clear_provider_api_key_from_config(store: &mut ConfigStore, provider: ProviderKind) {
    store.config.providers.for_provider_mut(provider).api_key = None;
    if provider == ProviderKind::Deepseek {
        store.config.api_key = None;
    }
}

fn provider_env_set(provider: ProviderKind) -> bool {
    provider_env_value(provider).is_some()
}

fn provider_env_vars(provider: ProviderKind) -> &'static [&'static str] {
    match provider {
        ProviderKind::Deepseek => &["DEEPSEEK_API_KEY"],
        ProviderKind::Openrouter => &["OPENROUTER_API_KEY"],
        ProviderKind::XiaomiMimo => &["XIAOMI_MIMO_API_KEY", "XIAOMI_API_KEY", "MIMO_API_KEY"],
        ProviderKind::Novita => &["NOVITA_API_KEY"],
        ProviderKind::NvidiaNim => &["NVIDIA_API_KEY", "NVIDIA_NIM_API_KEY", "DEEPSEEK_API_KEY"],
        ProviderKind::Fireworks => &["FIREWORKS_API_KEY"],
        ProviderKind::Siliconflow => &["SILICONFLOW_API_KEY"],
        ProviderKind::Moonshot => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        ProviderKind::Sglang => &["SGLANG_API_KEY"],
        ProviderKind::Vllm => &["VLLM_API_KEY"],
        ProviderKind::Ollama => &["OLLAMA_API_KEY"],
        ProviderKind::Anthropic => &["ANTHROPIC_API_KEY"],
        ProviderKind::Openai => &["OPENAI_API_KEY"],
        ProviderKind::Atlascloud => &["ATLASCLOUD_API_KEY"],
        ProviderKind::Volcengine => &[
            "VOLCENGINE_API_KEY",
            "VOLCENGINE_ARK_API_KEY",
            "ARK_API_KEY",
        ],
        ProviderKind::WanjieArk => &[
            "WANJIE_ARK_API_KEY",
            "WANJIE_API_KEY",
            "WANJIE_MAAS_API_KEY",
        ],
    }
}

fn provider_env_value(provider: ProviderKind) -> Option<(&'static str, String)> {
    provider_env_vars(provider).iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (*var, value))
    })
}

fn provider_config_api_key(store: &ConfigStore, provider: ProviderKind) -> Option<&str> {
    let slot = store
        .config
        .providers
        .for_provider(provider)
        .api_key
        .as_deref();
    let root = (provider == ProviderKind::Deepseek)
        .then_some(store.config.api_key.as_deref())
        .flatten();
    slot.or(root).filter(|v| !v.trim().is_empty())
}

fn provider_config_set(store: &ConfigStore, provider: ProviderKind) -> bool {
    provider_config_api_key(store, provider).is_some()
}

fn provider_keyring_api_key(secrets: &Secrets, provider: ProviderKind) -> Option<String> {
    secrets
        .get(provider_slot(provider))
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

fn provider_keyring_set(secrets: &Secrets, provider: ProviderKind) -> bool {
    provider_keyring_api_key(secrets, provider).is_some()
}

fn clear_provider_api_key_from_keyring(secrets: &Secrets, provider: ProviderKind) {
    let _ = secrets.delete(provider_slot(provider));
}

fn auth_status_lines(store: &ConfigStore, secrets: &Secrets) -> Vec<String> {
    let provider = store.config.provider;
    let config_key = provider_config_api_key(store, provider);
    let keyring_key = provider_keyring_api_key(secrets, provider);
    let env_key = provider_env_value(provider);

    let active_source = if config_key.is_some() {
        "config"
    } else if keyring_key.is_some() {
        "secret store"
    } else if env_key.is_some() {
        "env"
    } else {
        "missing"
    };
    let active_last4 = config_key
        .map(last4_label)
        .or_else(|| keyring_key.as_deref().map(last4_label))
        .or_else(|| env_key.as_ref().map(|(_, value)| last4_label(value)));
    let active_label = active_last4
        .map(|last4| format!("{active_source} (last4: {last4})"))
        .unwrap_or_else(|| active_source.to_string());

    let env_var_label = env_key
        .as_ref()
        .map(|(name, _)| (*name).to_string())
        .unwrap_or_else(|| provider_env_vars(provider).join("/"));
    let env_status = env_key
        .as_ref()
        .map(|(_, value)| format!("set, last4: {}", last4_label(value)))
        .unwrap_or_else(|| "unset".to_string());

    vec![
        format!("provider: {}", provider.as_str()),
        format!(
            "auth mode: {}",
            store.config.auth_mode.as_deref().unwrap_or("api_key")
        ),
        format!("active source: {active_label}"),
        "lookup order: config -> secret store -> env".to_string(),
        format!(
            "config file: {} ({})",
            store.path().display(),
            source_status(config_key, "missing")
        ),
        format!(
            "secret store: {} ({})",
            secrets.backend_name(),
            source_status(keyring_key.as_deref(), "missing")
        ),
        format!("env var: {env_var_label} ({env_status})"),
    ]
}

fn source_status(value: Option<&str>, missing_label: &str) -> String {
    value
        .map(|v| format!("set, last4: {}", last4_label(v)))
        .unwrap_or_else(|| missing_label.to_string())
}

fn last4_label(value: &str) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "<redacted>".to_string();
    }
    let last4: String = chars[chars.len() - 4..].iter().collect();
    format!("...{last4}")
}

pub(crate) fn run_auth_command(store: &mut ConfigStore, command: AuthCommand) -> Result<()> {
    run_auth_command_with_secrets(store, command, &Secrets::auto_detect())
}

fn run_auth_command_with_secrets(
    store: &mut ConfigStore,
    command: AuthCommand,
    secrets: &Secrets,
) -> Result<()> {
    match command {
        AuthCommand::Status => {
            for line in auth_status_lines(store, secrets) {
                println!("{line}");
            }
            Ok(())
        }
        AuthCommand::Set {
            provider,
            api_key,
            api_key_stdin,
        } => {
            let provider: ProviderKind = provider.into();
            let slot = provider_slot(provider);
            if provider == ProviderKind::Ollama && api_key.is_none() && !api_key_stdin {
                store.config.provider = provider;
                let provider_cfg = store.config.providers.for_provider_mut(provider);
                if provider_cfg.base_url.is_none() {
                    provider_cfg.base_url = Some("http://localhost:11434/v1".to_string());
                }
                store.save()?;
                println!(
                    "configured {slot} provider in {} (API key optional)",
                    store.path().display()
                );
                return Ok(());
            }
            let api_key = match (api_key, api_key_stdin) {
                (Some(v), _) => v,
                (None, true) => read_api_key_from_stdin()?,
                (None, false) => prompt_api_key(slot)?,
            };
            write_provider_api_key_to_config(store, provider, &api_key);
            let keyring_saved = write_provider_api_key_to_keyring(secrets, provider, &api_key);
            store.save()?;
            // Don't print the key. Don't echo length.
            if keyring_saved {
                println!(
                    "saved API key for {slot} to {} and {}",
                    store.path().display(),
                    secrets.backend_name()
                );
            } else {
                println!("saved API key for {slot} to {}", store.path().display());
            }
            Ok(())
        }
        AuthCommand::Get { provider } => {
            let provider: ProviderKind = provider.into();
            let slot = provider_slot(provider);
            let in_file = provider_config_set(store, provider);
            let in_keyring = !in_file && provider_keyring_set(secrets, provider);
            let in_env = provider_env_set(provider);
            // Report the highest-priority source that has it.
            let source = if in_file {
                Some("config-file")
            } else if in_keyring {
                Some("secret-store")
            } else if in_env {
                Some("env")
            } else {
                None
            };
            match source {
                Some(source) => println!("{slot}: set (source: {source})"),
                None => println!("{slot}: not set"),
            }
            Ok(())
        }
        AuthCommand::Clear { provider } => {
            let provider: ProviderKind = provider.into();
            let slot = provider_slot(provider);
            clear_provider_api_key_from_config(store, provider);
            clear_provider_api_key_from_keyring(secrets, provider);
            store.save()?;
            println!("cleared API key for {slot} from config and secret store");
            Ok(())
        }
        AuthCommand::List => {
            println!("provider     config store env  active");
            let active_provider = store.config.provider;
            for provider in PROVIDER_LIST {
                let slot = provider_slot(provider);
                let file = provider_config_set(store, provider);
                let keyring = (provider == active_provider && !file)
                    .then(|| provider_keyring_set(secrets, provider));
                let env = provider_env_set(provider);
                let active = if file {
                    "config"
                } else if keyring == Some(true) {
                    "store"
                } else if env {
                    "env"
                } else {
                    "missing"
                };
                println!(
                    "{slot:<12}  {}     {}      {}   {active}",
                    yes_no(file),
                    keyring_status_short(keyring),
                    yes_no(env)
                );
            }
            Ok(())
        }
        AuthCommand::Migrate { dry_run } => run_auth_migrate(store, secrets, dry_run),
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no " }
}

fn keyring_status_short(state: Option<bool>) -> &'static str {
    match state {
        Some(true) => "yes",
        Some(false) => "no ",
        None => "n/a",
    }
}

fn prompt_api_key(slot: &str) -> Result<String> {
    use std::io::{IsTerminal, Write};
    eprint!("Enter API key for {slot}: ");
    io::stderr().flush().ok();
    if !io::stdin().is_terminal() {
        // Non-interactive: read directly without prompting twice.
        return read_api_key_from_stdin();
    }
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .context("failed to read API key from stdin")?;
    let key = buf.trim().to_string();
    if key.is_empty() {
        bail!("empty API key provided");
    }
    Ok(key)
}

pub(crate) fn run_config_command(store: &mut ConfigStore, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Get { key } => {
            if let Some(value) = store.config.get_display_value(&key) {
                println!("{value}");
                return Ok(());
            }
            bail!("key not found: {key}");
        }
        ConfigCommand::Set { key, value } => {
            store.config.set_value(&key, &value)?;
            store.save()?;
            println!("set {key}");
            Ok(())
        }
        ConfigCommand::Unset { key } => {
            store.config.unset_value(&key)?;
            store.save()?;
            println!("unset {key}");
            Ok(())
        }
        ConfigCommand::List => {
            for (key, value) in store.config.list_values() {
                println!("{key} = {value}");
            }
            Ok(())
        }
        ConfigCommand::Path => {
            println!("{}", store.path().display());
            Ok(())
        }
    }
}

pub(crate) fn run_model_command(command: ModelCommand) -> Result<()> {
    let registry = ModelRegistry::default();
    match command {
        ModelCommand::List { provider } => {
            let filter = provider.map(ProviderKind::from);
            for model in registry.list().into_iter().filter(|m| match filter {
                Some(p) => m.provider == p,
                None => true,
            }) {
                println!("{} ({})", model.id, model.provider.as_str());
            }
            Ok(())
        }
        ModelCommand::Resolve { model, provider } => {
            let resolved = registry.resolve(model.as_deref(), provider.map(ProviderKind::from));
            println!("requested: {}", resolved.requested.unwrap_or_default());
            println!("resolved: {}", resolved.resolved.id);
            println!("provider: {}", resolved.resolved.provider.as_str());
            println!("used_fallback: {}", resolved.used_fallback);
            Ok(())
        }
    }
}

pub(crate) fn run_thread_command(command: ThreadCommand) -> Result<()> {
    let state = StateStore::open(None)?;
    match command {
        ThreadCommand::List { all, limit } => {
            let threads = state.list_threads(ThreadListFilters {
                include_archived: all,
                limit,
            })?;
            for thread in threads {
                println!(
                    "{} | {} | {} | {}",
                    thread.id,
                    thread
                        .name
                        .clone()
                        .unwrap_or_else(|| "(unnamed)".to_string()),
                    thread.model_provider,
                    thread.cwd.display()
                );
            }
            Ok(())
        }
        ThreadCommand::Read { thread_id } => {
            let thread = state.get_thread(&thread_id)?;
            println!("{}", serde_json::to_string_pretty(&thread)?);
            Ok(())
        }
        ThreadCommand::Resume { thread_id } => {
            println!("resume this thread interactively: codesmith resume {thread_id}");
            Ok(())
        }
        ThreadCommand::Fork { thread_id } => {
            println!("fork this thread into a new session: codesmith fork {thread_id}");
            Ok(())
        }
        ThreadCommand::Archive { thread_id } => {
            state.mark_archived(&thread_id)?;
            println!("archived {thread_id}");
            Ok(())
        }
        ThreadCommand::Unarchive { thread_id } => {
            state.mark_unarchived(&thread_id)?;
            println!("unarchived {thread_id}");
            Ok(())
        }
        ThreadCommand::SetName { thread_id, name } => {
            let mut thread = state
                .get_thread(&thread_id)?
                .with_context(|| format!("thread not found: {thread_id}"))?;
            thread.name = Some(name);
            thread.updated_at = chrono::Utc::now().timestamp();
            state.upsert_thread(&thread)?;
            println!("renamed {thread_id}");
            Ok(())
        }
        ThreadCommand::ClearName { thread_id } => {
            let mut thread = state
                .get_thread(&thread_id)?
                .with_context(|| format!("thread not found: {thread_id}"))?;
            thread.name = None;
            thread.updated_at = chrono::Utc::now().timestamp();
            state.upsert_thread(&thread)?;
            println!("cleared name for {thread_id}");
            Ok(())
        }
    }
}

pub(crate) fn run_sandbox_check(command: String, ask: ApprovalModeArg) -> Result<()> {
    {
        let engine = ExecPolicyEngine::new(Vec::new(), vec!["rm -rf".to_string()]);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let decision = engine.check(ExecPolicyContext {
            command: &command,
            cwd: &cwd.display().to_string(),
            tool: Some("exec_shell"),
            path: None,
            ask_for_approval: ask.into(),
            sandbox_mode: Some("workspace-write"),
        })?;
        println!("{}", serde_json::to_string_pretty(&decision)?);
        Ok(())
    }
}

pub(crate) fn run_app_server_command(args: AppServerArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;
    if args.stdio {
        return runtime.block_on(run_app_server_stdio(args.config));
    }
    let listen: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| {
            format!(
                "invalid app-server listen address {}:{}",
                args.host, args.port
            )
        })?;
    runtime.block_on(run_app_server(AppServerOptions {
        listen,
        config_path: args.config,
        auth_token: args.auth_token.or_else(app_server_token_from_env),
        insecure_no_auth: args.insecure_no_auth,
        cors_origins: args.cors_origin,
    }))
}

fn app_server_token_from_env() -> Option<String> {
    std::env::var("CODESMITH_APP_SERVER_TOKEN").ok()
}

pub(crate) fn run_mcp_server_command(store: &mut ConfigStore) -> Result<()> {
    let persisted = load_mcp_server_definitions(store);
    let updated = run_stdio_server(persisted)?;
    persist_mcp_server_definitions(store, &updated)
}

fn load_mcp_server_definitions(store: &ConfigStore) -> Vec<McpServerDefinition> {
    let Some(raw) = store.config.get_value(MCP_SERVER_DEFINITIONS_KEY) else {
        return Vec::new();
    };

    match parse_mcp_server_definitions(&raw) {
        Ok(definitions) => definitions,
        Err(err) => {
            eprintln!(
                "warning: failed to parse persisted MCP server definitions ({MCP_SERVER_DEFINITIONS_KEY}): {err}"
            );
            Vec::new()
        }
    }
}

fn parse_mcp_server_definitions(raw: &str) -> Result<Vec<McpServerDefinition>> {
    if let Ok(parsed) = serde_json::from_str::<Vec<McpServerDefinition>>(raw) {
        return Ok(parsed);
    }

    let unwrapped: String = serde_json::from_str(raw)
        .with_context(|| format!("invalid JSON payload at key {MCP_SERVER_DEFINITIONS_KEY}"))?;
    serde_json::from_str::<Vec<McpServerDefinition>>(&unwrapped).with_context(|| {
        format!("invalid MCP server definition list in key {MCP_SERVER_DEFINITIONS_KEY}")
    })
}

fn persist_mcp_server_definitions(
    store: &mut ConfigStore,
    definitions: &[McpServerDefinition],
) -> Result<()> {
    let encoded =
        serde_json::to_string(definitions).context("failed to encode MCP server definitions")?;
    store
        .config
        .set_value(MCP_SERVER_DEFINITIONS_KEY, &encoded)?;
    store.save()
}

pub(crate) fn run_metrics_command(args: MetricsArgs) -> Result<()> {
    let since = match args.since.as_deref() {
        Some(s) => {
            Some(metrics::parse_since(s).with_context(|| format!("invalid --since value: {s:?}"))?)
        }
        None => None,
    };
    metrics::run(metrics::MetricsArgs {
        json: args.json,
        since,
    })
}

fn read_api_key_from_stdin() -> Result<String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("failed to read api key from stdin")?;
    let key = input.trim().to_string();
    if key.is_empty() {
        bail!("empty API key provided");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use clap::error::ErrorKind;

    fn parse_ok(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).unwrap_or_else(|err| panic!("parse failed for {argv:?}: {err}"))
    }

    fn help_for(argv: &[&str]) -> String {
        let err = Cli::try_parse_from(argv).expect_err("expected --help to short-circuit parsing");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        err.to_string()
    }

    #[test]
    fn clap_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    // Regression for #767: `run_cli` prints the full anyhow chain so users
    // see the underlying TOML parser error (line/column, expected token)
    // instead of just the top-level "failed to parse config at <path>"
    // wrapper. anyhow's bare `Display` impl drops the chain — pin both
    // pieces here so a future refactor of the printing path doesn't
    // silently regress.
    #[test]
    fn anyhow_chain_surfaces_toml_parse_cause() {
        use anyhow::Context;
        let inner = anyhow::anyhow!("TOML parse error at line 1, column 20");
        let err = Err::<(), _>(inner)
            .context("failed to parse config at C:\\Users\\test\\.codesmith\\config.toml")
            .unwrap_err();

        // What `eprintln!("error: {err}")` prints (top context only).
        assert_eq!(
            err.to_string(),
            "failed to parse config at C:\\Users\\test\\.codesmith\\config.toml",
        );

        // What the `for cause in err.chain().skip(1)` loop iterates over.
        let causes: Vec<String> = err.chain().skip(1).map(ToString::to_string).collect();
        assert_eq!(causes, vec!["TOML parse error at line 1, column 20"]);
    }

    #[test]
    fn parses_config_command_matrix() {
        let cli = parse_ok(&["codesmith", "config", "get", "provider"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Config(crate::ConfigArgs {
                command: ConfigCommand::Get { ref key }
            })) if key == "provider"
        ));

        let cli = parse_ok(&["codesmith", "config", "set", "model", "deepseek-v4-flash"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Config(crate::ConfigArgs {
                command: ConfigCommand::Set { ref key, ref value }
            })) if key == "model" && value == "deepseek-v4-flash"
        ));

        let cli = parse_ok(&["codesmith", "config", "unset", "model"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Config(crate::ConfigArgs {
                command: ConfigCommand::Unset { ref key }
            })) if key == "model"
        ));

        assert!(matches!(
            parse_ok(&["codesmith", "config", "list"]).command,
            Some(main_root::Commands::Config(crate::ConfigArgs {
                command: ConfigCommand::List
            }))
        ));
        assert!(matches!(
            parse_ok(&["codesmith", "config", "path"]).command,
            Some(main_root::Commands::Config(crate::ConfigArgs {
                command: ConfigCommand::Path
            }))
        ));
    }

    #[test]
    fn parses_update_beta_flag() {
        let cli = parse_ok(&["codesmith", "update"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Update(UpdateArgs {
                beta: false,
                check: false,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codesmith", "update", "--beta"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Update(UpdateArgs {
                beta: true,
                check: false,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codesmith", "update", "--check"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Update(UpdateArgs {
                beta: false,
                check: true,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codesmith", "update", "--proxy", "socks5://127.0.0.1:1080"]);
        let Some(main_root::Commands::Update(args)) = cli.command else {
            panic!("expected update command");
        };
        assert!(!args.beta);
        assert!(!args.check);
        assert_eq!(args.proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn parses_model_command_matrix() {
        let cli = parse_ok(&["codesmith", "model", "list"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Model(ModelArgs {
                command: ModelCommand::List { provider: None }
            }))
        ));

        let cli = parse_ok(&["codesmith", "model", "list", "--provider", "openai"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Model(ModelArgs {
                command: ModelCommand::List {
                    provider: Some(ProviderArg::Openai)
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "model", "resolve", "deepseek-v4-flash"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Model(ModelArgs {
                command: ModelCommand::Resolve {
                    model: Some(ref model),
                    provider: None
                }
            })) if model == "deepseek-v4-flash"
        ));

        let cli = parse_ok(&[
            "codesmith",
            "model",
            "resolve",
            "--provider",
            "deepseek",
            "deepseek-v4-pro",
        ]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Model(ModelArgs {
                command: ModelCommand::Resolve {
                    model: Some(ref model),
                    provider: Some(ProviderArg::Deepseek)
                }
            })) if model == "deepseek-v4-pro"
        ));
    }

    #[test]
    fn parses_thread_command_matrix() {
        let cli = parse_ok(&["codesmith", "thread", "list", "--all", "--limit", "50"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::List {
                    all: true,
                    limit: Some(50)
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "thread", "read", "thread-1"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::Read { ref thread_id }
            })) if thread_id == "thread-1"
        ));

        let cli = parse_ok(&["codesmith", "thread", "resume", "thread-2"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::Resume { ref thread_id }
            })) if thread_id == "thread-2"
        ));

        let cli = parse_ok(&["codesmith", "thread", "fork", "thread-3"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::Fork { ref thread_id }
            })) if thread_id == "thread-3"
        ));

        let cli = parse_ok(&["codesmith", "thread", "archive", "thread-4"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::Archive { ref thread_id }
            })) if thread_id == "thread-4"
        ));

        let cli = parse_ok(&["codesmith", "thread", "unarchive", "thread-5"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::Unarchive { ref thread_id }
            })) if thread_id == "thread-5"
        ));

        let cli = parse_ok(&["codesmith", "thread", "set-name", "thread-6", "My Thread"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::SetName {
                    ref thread_id,
                    ref name
                }
            })) if thread_id == "thread-6" && name == "My Thread"
        ));

        let cli = parse_ok(&["codesmith", "thread", "clear-name", "thread-7"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Thread(ThreadArgs {
                command: ThreadCommand::ClearName { ref thread_id }
            })) if thread_id == "thread-7"
        ));
    }

    #[test]
    fn deepseek_login_writes_shared_config_and_preserves_tui_defaults() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "codesmith-cli-login-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let secrets = no_keyring_secrets();

        run_login_command_with_secrets(
            &mut store,
            LoginArgs {
                provider: Some(ProviderArg::Deepseek),
                api_key: Some("sk-test".to_string()),
            },
            &secrets,
        )
        .expect("login should write config");

        assert_eq!(store.config.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            store.config.providers.deepseek.api_key.as_deref(),
            Some("sk-test")
        );
        assert_eq!(
            store.config.default_text_model.as_deref(),
            Some("deepseek-v4-pro")
        );
        let saved = std::fs::read_to_string(&path).expect("config should be written");
        assert!(saved.contains("api_key = \"sk-test\""));
        assert!(saved.contains("default_text_model = \"deepseek-v4-pro\""));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parses_auth_subcommand_matrix() {
        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "deepseek"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Deepseek,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&[
            "codesmith",
            "auth",
            "set",
            "--provider",
            "openrouter",
            "--api-key-stdin",
        ]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Openrouter,
                    api_key: None,
                    api_key_stdin: true,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "get", "--provider", "novita"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderArg::Novita
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "clear", "--provider", "nvidia-nim"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Clear {
                    provider: ProviderArg::NvidiaNim
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "fireworks"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Fireworks,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "siliconflow"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Siliconflow,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "moonshot"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Moonshot,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "wanjie-ark"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::WanjieArk,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "get", "--provider", "sglang"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderArg::Sglang
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "get", "--provider", "vllm"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderArg::Vllm
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "set", "--provider", "ollama"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderArg::Ollama,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "list"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::List
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "migrate"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Migrate { dry_run: false }
            }))
        ));

        let cli = parse_ok(&["codesmith", "auth", "migrate", "--dry-run"]);
        assert!(matches!(
            cli.command,
            Some(main_root::Commands::Auth(AuthArgs {
                command: AuthCommand::Migrate { dry_run: true }
            }))
        ));
    }

    #[test]
    fn auth_set_writes_to_shared_config_file() {
        use codesmith_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "codesmith-cli-auth-set-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderArg::Deepseek,
                api_key: Some("sk-keyring".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("set should succeed");

        assert_eq!(store.config.api_key.as_deref(), Some("sk-keyring"));
        assert_eq!(
            store.config.providers.deepseek.api_key.as_deref(),
            Some("sk-keyring")
        );
        let saved = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(saved.contains("api_key = \"sk-keyring\""));
        assert_eq!(
            inner.get("deepseek").unwrap().as_deref(),
            Some("sk-keyring")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_set_ollama_accepts_empty_key_and_records_base_url() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "codesmith-cli-auth-ollama-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let secrets = no_keyring_secrets();

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderArg::Ollama,
                api_key: None,
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("ollama auth set should not require a key");

        assert_eq!(store.config.provider, ProviderKind::Ollama);
        assert_eq!(
            store.config.providers.ollama.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(store.config.providers.ollama.api_key, None);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_status_and_list_only_probe_active_provider_keyring() {
        use codesmith_secrets::{KeyringStore, SecretsError};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct RecordingStore {
            gets: Mutex<Vec<String>>,
        }

        impl KeyringStore for RecordingStore {
            fn get(&self, key: &str) -> Result<Option<String>, SecretsError> {
                self.gets.lock().unwrap().push(key.to_string());
                Ok(None)
            }

            fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
                Ok(())
            }

            fn delete(&self, _key: &str) -> Result<(), SecretsError> {
                Ok(())
            }

            fn backend_name(&self) -> &'static str {
                "recording"
            }
        }

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "codesmith-cli-auth-active-keyring-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        let inner = Arc::new(RecordingStore::default());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(&mut store, AuthCommand::Status, &secrets)
            .expect("status should succeed");
        run_auth_command_with_secrets(&mut store, AuthCommand::List, &secrets)
            .expect("list should succeed");

        assert_eq!(
            inner.gets.lock().unwrap().as_slice(),
            ["deepseek", "deepseek"]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn logout_removes_plaintext_provider_keys() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-logout-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.api_key = Some("sk-stale".to_string());
        store.config.providers.deepseek.api_key = Some("sk-stale".to_string());
        store.config.providers.fireworks.api_key = Some("fw-stale".to_string());
        store.save().unwrap();

        let secrets = no_keyring_secrets();

        run_logout_command_with_secrets(&mut store, &secrets).expect("logout should succeed");

        assert!(store.config.api_key.is_none());
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert!(store.config.providers.fireworks.api_key.is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_migrate_moves_plaintext_keys_into_keyring_and_strips_file() {
        use codesmith_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-migrate-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.api_key = Some("sk-deep".to_string());
        store.config.providers.deepseek.api_key = Some("sk-deep".to_string());
        store.config.providers.openrouter.api_key = Some("or-key".to_string());
        store.config.providers.novita.api_key = Some("nv-key".to_string());
        store.save().unwrap();

        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Migrate { dry_run: false },
            &secrets,
        )
        .expect("migrate should succeed");

        assert_eq!(inner.get("deepseek").unwrap(), Some("sk-deep".to_string()));
        assert_eq!(inner.get("openrouter").unwrap(), Some("or-key".to_string()));
        assert_eq!(inner.get("novita").unwrap(), Some("nv-key".to_string()));

        // Config file must no longer contain the api keys.
        assert!(store.config.api_key.is_none());
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert!(store.config.providers.openrouter.api_key.is_none());
        assert!(store.config.providers.novita.api_key.is_none());

        let saved = std::fs::read_to_string(&path).expect("config exists post-migrate");
        assert!(!saved.contains("sk-deep"), "plaintext leaked: {saved}");
        assert!(!saved.contains("or-key"), "plaintext leaked: {saved}");
        assert!(!saved.contains("nv-key"), "plaintext leaked: {saved}");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_migrate_dry_run_does_not_modify_anything() {
        use codesmith_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-migrate-dry-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.providers.openrouter.api_key = Some("or-stay".to_string());
        store.save().unwrap();

        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(&mut store, AuthCommand::Migrate { dry_run: true }, &secrets)
            .expect("dry-run should succeed");

        assert_eq!(inner.get("openrouter").unwrap(), None);
        assert_eq!(
            store.config.providers.openrouter.api_key.as_deref(),
            Some("or-stay")
        );

        let _ = std::fs::remove_file(path);
    }

    // §D2 slice 46 — `--custom-provider <id>` env forwarding + guards.

    #[test]
    fn custom_provider_conflicts_with_provider_flag() {
        // clap `conflicts_with = "provider"` rejects the combination at parse.
        let err = Cli::try_parse_from([
            "deepseek",
            "--provider",
            "openai",
            "--custom-provider",
            "acme",
        ])
        .expect_err("conflicting flags must be rejected");
        assert!(
            err.to_string().contains("cannot be used with"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn custom_provider_flag_reaches_config_and_fails_loud_on_unknown_id() {
        // Before the fold in `load_config_from_cli_with_exec_model` the flag
        // parsed and was dropped, so an unknown id loaded a config silently.
        let _guard = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[[providers.custom]]\nid = \"other\"\nbase_url = \"https://gateway.example/v1\"\n",
        )
        .expect("write config");
        let cli = parse_ok(&[
            "codesmith",
            "--config",
            config_path.to_str().expect("utf8 path"),
            "--custom-provider",
            "acme",
        ]);
        let err = main_root::load_config_from_cli(&cli).expect_err("unknown custom id must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("custom_provider 'acme' has no matching [[providers.custom]] entry"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn known_subcommands_still_parse_before_prompt_tail() {
        let cli = parse_ok(&["codesmith", "doctor"]);

        assert!(cli.prompt.is_empty());
        assert!(matches!(cli.command, Some(main_root::Commands::Doctor(_))));
    }

    #[test]
    fn unknown_subcommand_error_uses_clap_usage_shape() {
        let err = unknown_subcommand_error("docotr");

        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
        let rendered = err.to_string();
        for token in [
            "error: unrecognized subcommand 'docotr'",
            "tip: to send this as a prompt, run: codesmith -p docotr",
            "tip: run `codesmith --help` to see all commands",
            "Usage:",
            "[OPTIONS] [COMMAND]",
        ] {
            assert!(
                rendered.contains(token),
                "missing {token:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn version_and_docker_are_subcommands() {
        assert!(matches!(
            parse_ok(&["codesmith", "version"]).command,
            Some(main_root::Commands::Version)
        ));
        assert!(matches!(
            parse_ok(&["codesmith", "docker"]).command,
            Some(main_root::Commands::Docker)
        ));

        let rendered = Cli::command().render_version();
        assert!(rendered.starts_with("codesmith "), "{rendered}");
        assert!(
            DOCKER_USAGE.contains("ghcr.io/camilesing/codesmith"),
            "docker help must name the published image"
        );
    }

    #[test]
    fn root_help_surface_contains_expected_subcommands_and_globals() {
        let rendered = help_for(&["deepseek", "--help"]);

        for token in [
            "run",
            "doctor",
            "models",
            "sessions",
            "resume",
            "setup",
            "login",
            "logout",
            "auth",
            "mcp-server",
            "config",
            "model",
            "thread",
            "sandbox",
            "app-server",
            "completion",
            "metrics",
            "docker",
            "version",
            "--provider",
            "--model",
            "--config",
            "--profile",
            "--output-mode",
            "--log-level",
            "--telemetry",
            "--base-url",
            "--api-key",
            "--approval-policy",
            "--sandbox-mode",
            "--skip-onboarding",
            "--continue",
            "--prompt",
        ] {
            assert!(
                rendered.contains(token),
                "expected help to contain token: {token}"
            );
        }
        // TUI-era terminal-chrome flags are gone with the ratatui frontend.
        for gone in ["--no-alt-screen", "--mouse-capture", "--no-mouse-capture"] {
            assert!(
                !rendered.contains(gone),
                "expected help to no longer contain token: {gone}"
            );
        }
    }

    #[test]
    fn subcommand_help_surfaces_are_stable() {
        let cases = [
            ("config", vec!["get", "set", "unset", "list", "path"]),
            ("model", vec!["list", "resolve"]),
            (
                "thread",
                vec![
                    "list",
                    "read",
                    "resume",
                    "fork",
                    "archive",
                    "unarchive",
                    "set-name",
                    "clear-name",
                ],
            ),
            ("sandbox", vec!["check"]),
            (
                "exec",
                vec![
                    "--auto",
                    "--json",
                    "--resume",
                    "--session-id",
                    "--continue",
                    "--output-format",
                    "stream-json",
                ],
            ),
            (
                "app-server",
                vec!["--host", "--port", "--config", "--stdio"],
            ),
            (
                "completion",
                vec![
                    "<SHELL>",
                    "bash",
                    "source <(codesmith completion bash)",
                    "~/.local/share/bash-completion/completions/codesmith",
                    "fpath=(~/.zfunc $fpath)",
                    "codesmith completion fish > ~/.config/fish/completions/codesmith.fish",
                    "codesmith completion powershell | Out-String | Invoke-Expression",
                ],
            ),
            ("metrics", vec!["--json", "--since"]),
        ];

        for (subcommand, expected_tokens) in cases {
            let argv = ["deepseek", subcommand, "--help"];
            let rendered = help_for(&argv);
            for token in expected_tokens {
                assert!(
                    rendered.contains(token),
                    "expected help for `{subcommand}` to include `{token}`"
                );
            }
        }
    }
}
