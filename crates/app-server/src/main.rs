use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use codesmith_app_server::{AppServerOptions, run};

#[derive(Debug, Parser)]
#[command(
    name = "codesmith-app-server",
    about = "Run the CodeSmith app-server transport"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8787)]
    port: u16,
    #[arg(long)]
    config: Option<PathBuf>,
    /// Auth token. Prefer the CODESMITH_APP_SERVER_TOKEN env var: argv is
    /// visible to every process on the host (`ps`).
    #[arg(long = "auth-token")]
    auth_token: Option<String>,
    #[arg(long, default_value_t = false)]
    insecure_no_auth: bool,
    #[arg(long = "cors-origin")]
    cors_origin: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let listen: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", cli.host, cli.port))?;
    // Env wins over the flag: the flag value is visible in `ps` output, so a
    // caller that set both almost certainly wants the env token used.
    let auth_token = app_server_token_from_env().or_else(|| {
        if cli.auth_token.is_some() {
            eprintln!(
                "warning: auth token passed via --auth-token is visible in the process list; \
                 prefer the CODESMITH_APP_SERVER_TOKEN environment variable"
            );
        }
        cli.auth_token
    });
    run(AppServerOptions {
        listen,
        config_path: cli.config,
        auth_token,
        insecure_no_auth: cli.insecure_no_auth,
        cors_origins: cli.cors_origin,
    })
    .await
}

fn app_server_token_from_env() -> Option<String> {
    std::env::var("CODESMITH_APP_SERVER_TOKEN")
        .ok()
        .or_else(|| std::env::var("DEEPSEEK_APP_SERVER_TOKEN").ok())
}
