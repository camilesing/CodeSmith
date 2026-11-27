//! `/import [claude|cursor|codex|gemini] [--dry-run] [--yes]` — bring another
//! agent's configuration over.
//!
//! What is worth importing here is narrow, and the scan says so rather than
//! claiming more: CodeSmith already reads most of the other tools' surfaces
//! natively, so the command's job is to *report* that and to copy over the
//! one thing it does not share — **MCP servers**.
//!
//! | Surface | Where it stands |
//! | :- | :- |
//! | `CLAUDE.md`, `.claude/instructions.md`, `AGENTS.md` | already a memory tier — nothing to import |
//! | `.claude/commands/*.md`, `.cursor/commands/*.md` | already scanned as user commands |
//! | `~/.claude/skills`, `~/.agents/skills` | already scanned by the skill registry |
//! | MCP servers (`mcpServers` in Claude/Cursor/Gemini, `[mcp_servers]` in Codex) | **imported** into `~/.codesmith/mcp.json`, stdio and URL alike |
//! | sub-agent definitions (`.claude/agents/*.md`) | no equivalent: CodeSmith sub-agents are spawned per task |
//! | `GEMINI.md`, `~/.codex/AGENTS.md` | not a memory tier; the scan names the file to copy |
//!
//! Nothing is written without `--yes`, which mirrors Claude Code's own
//! non-interactive behaviour (it prints what it found and the command that
//! confirms the import).
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Stdio and URL servers come over (`command`/`args`/`env`, and
//!   `url`/`transport`/`headers`; Gemini's `httpUrl`, and Codex's
//!   `http_headers` and `bearer_token_env_var`, are read under those). An
//!   entry with neither is skipped.
//!   Per-server timeouts and OAuth blocks are not carried: they are
//!   CodeSmith-specific shapes the source tools do not have.
//! - A server whose name already exists in `~/.codesmith/mcp.json` is left
//!   alone (the existing entry wins) and reported as skipped.
//! - Secrets are not lifted: `env` values are copied as written, so a token
//!   hard-coded in another tool's config lands in the CodeSmith config too.
//!   That is the same exposure the source file already had.
//! - An imported entry is written in this client's full shape (every field,
//!   serde defaults included) rather than the minimal shape the source file
//!   used, so the first import into an empty `mcp.json` adds more lines than
//!   it strictly needs to.

use std::path::{Path, PathBuf};

use colored::Colorize;

use super::ReplState;

/// One source agent's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Claude,
    Cursor,
    Codex,
    Gemini,
}

impl Source {
    /// Parse the command word.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "cc" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "codex" | "openai-codex" => Some(Self::Codex),
            "gemini" | "gemini-cli" => Some(Self::Gemini),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    /// Every source, in scan order.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Claude, Self::Cursor, Self::Codex, Self::Gemini]
    }

    /// The MCP config files this source keeps, in read order.
    #[must_use]
    fn mcp_paths(self, home: Option<&Path>, workspace: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(home) = home {
            match self {
                Self::Claude => {
                    paths.push(home.join(".claude.json"));
                    paths.push(home.join(".claude").join("settings.json"));
                }
                Self::Cursor => paths.push(home.join(".cursor").join("mcp.json")),
                Self::Codex => paths.push(home.join(".codex").join("config.toml")),
                Self::Gemini => paths.push(home.join(".gemini").join("settings.json")),
            }
        }
        match self {
            Self::Claude => paths.push(workspace.join(".mcp.json")),
            Self::Cursor => paths.push(workspace.join(".cursor").join("mcp.json")),
            Self::Codex => paths.push(workspace.join(".codex").join("config.toml")),
            Self::Gemini => paths.push(workspace.join(".gemini").join("settings.json")),
        }
        paths
    }

    /// Instruction files of this source that CodeSmith does **not** read.
    #[must_use]
    fn extra_notes(self, home: Option<&Path>) -> Vec<String> {
        let Some(home) = home else {
            return Vec::new();
        };
        match self {
            Self::Gemini => vec![format!(
                "{} is not a memory tier — copy what you need into ~/.codesmith/AGENTS.md",
                home.join(".gemini").join("GEMINI.md").display()
            )],
            Self::Codex => vec![format!(
                "{} is not a memory tier — copy what you need into ~/.codesmith/AGENTS.md",
                home.join(".codex").join("AGENTS.md").display()
            )],
            Self::Claude => vec![
                "CLAUDE.md and .claude/instructions.md are already read as memory".to_string(),
                ".claude/commands/*.md are already scanned as user commands".to_string(),
                "~/.claude/skills is already scanned by the skill registry".to_string(),
                ".claude/agents/*.md has no equivalent — sub-agents are spawned per task"
                    .to_string(),
            ],
            Self::Cursor => {
                vec![".cursor/commands/*.md are already scanned as user commands".to_string()]
            }
        }
    }
}

/// One server found in a source, ready to write into `mcp.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedServer {
    pub name: String,
    /// Stdio servers only; `None` for a URL server.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// URL servers only; `None` for stdio.
    pub url: Option<String>,
    pub transport: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl ImportedServer {
    /// How the server is reached, for the report.
    #[must_use]
    pub fn endpoint(&self) -> String {
        match (&self.command, &self.url) {
            (Some(command), _) => command.clone(),
            (None, Some(url)) => url.clone(),
            (None, None) => "(none)".to_string(),
        }
    }

    /// The JSON object `mcp.json` stores for this server.
    ///
    /// Built as a value rather than a struct literal so serde's own defaults
    /// apply to the fields the source formats do not carry (timeouts,
    /// enabled/disabled, tool filters) — the same defaults a hand-written
    /// entry gets.
    #[must_use]
    pub fn to_config_value(&self) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if let Some(command) = &self.command {
            value.insert("command".to_string(), serde_json::json!(command));
            value.insert("args".to_string(), serde_json::json!(self.args));
            value.insert(
                "env".to_string(),
                serde_json::json!(
                    self.env
                        .iter()
                        .cloned()
                        .collect::<std::collections::HashMap<_, _>>()
                ),
            );
        }
        if let Some(url) = &self.url {
            value.insert("url".to_string(), serde_json::json!(url));
        }
        if let Some(transport) = &self.transport {
            value.insert("transport".to_string(), serde_json::json!(transport));
        }
        if !self.headers.is_empty() {
            value.insert(
                "headers".to_string(),
                serde_json::json!(
                    self.headers
                        .iter()
                        .cloned()
                        .collect::<std::collections::HashMap<_, _>>()
                ),
            );
        }
        serde_json::Value::Object(value)
    }
}

/// What one source contributed.
#[derive(Debug, Default)]
pub struct Found {
    pub servers: Vec<ImportedServer>,
    /// `(name, reason)` for entries that cannot be imported.
    pub skipped: Vec<(String, String)>,
    /// Files that were read, for the report.
    pub read_files: Vec<PathBuf>,
}

/// Keep a candidate unless it names neither a command nor a url.
///
/// A shape the source tools allow and this client cannot use is reported as
/// skipped rather than imported as an empty server.
fn usable(server: ImportedServer, skipped: &mut Vec<(String, String)>) -> Option<ImportedServer> {
    if server.command.is_none() && server.url.is_none() {
        skipped.push((server.name.clone(), "neither command nor url".to_string()));
        return None;
    }
    Some(server)
}

/// Extract importable servers from an MCP document.
///
/// Accepts the `mcpServers` mapping Claude, Cursor, and Gemini use, plus
/// Claude Code's project-scoped `projects."<path>".mcpServers` nesting
/// (`~/.claude.json` keeps most servers there). An entry naming neither a
/// command nor a url is skipped with a reason rather than imported as an
/// empty server.
#[must_use]
pub fn servers_from_json(document: &serde_json::Value) -> Found {
    let mut found = Found::default();
    let mut maps: Vec<&serde_json::Map<String, serde_json::Value>> = Vec::new();
    if let Some(map) = document
        .get("mcpServers")
        .or_else(|| document.get("servers"))
        .and_then(|value| value.as_object())
    {
        maps.push(map);
    }
    if let Some(projects) = document.get("projects").and_then(|value| value.as_object()) {
        for project in projects.values() {
            if let Some(map) = project
                .get("mcpServers")
                .or_else(|| project.get("servers"))
                .and_then(|value| value.as_object())
            {
                maps.push(map);
            }
        }
    }
    for map in maps {
        for (name, entry) in map {
            if let Some(server) = usable(imported_from_json(name, entry), &mut found.skipped) {
                found.servers.push(server);
            }
        }
    }
    found
}

/// Convert one JSON server entry. Non-string scalars in env/headers/args
/// are stringified rather than dropped: a stdio server losing a numeric
/// token from `env` launches without it, and dropping one positional arg
/// silently shifts every later flag onto the wrong position.
fn imported_from_json(name: &str, entry: &serde_json::Value) -> ImportedServer {
    let scalar = |value: &serde_json::Value| -> Option<String> {
        match value {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    };
    let strings = |field: &str| {
        entry
            .get(field)
            .and_then(|value| value.as_object())
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| scalar(value).map(|value| (key.clone(), value)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let args = entry
        .get("args")
        .and_then(|value| value.as_array())
        .map(|args| {
            // Arg position carries meaning: `filter_map(scalar)` dropped a
            // JSON `null` (or object/array) element and shifted everything
            // after it — `["--flag", null, "file"]` put `file` where the
            // flag's value belonged. Stringify non-scalars like serde does.
            args.iter()
                .map(|value| scalar(value).unwrap_or_else(|| value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    ImportedServer {
        name: name.to_string(),
        command: entry
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        args,
        env: strings("env"),
        // Gemini CLI spells a remote server's endpoint `httpUrl`.
        url: entry
            .get("url")
            .or_else(|| entry.get("httpUrl"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        transport: entry
            .get("transport")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        headers: strings("headers"),
    }
}

/// The same extraction for Codex's TOML `[mcp_servers.<name>]` tables.
#[must_use]
pub fn servers_from_toml(document: &str) -> Found {
    let mut found = Found::default();
    // `toml::from_str` parses a document; `Value::from_str` in toml 0.9
    // parses a single value and rejects a whole file ("unexpected content"),
    // which is how this silently imported nothing until the test caught it.
    let parsed = match toml::from_str::<toml::Value>(document) {
        Ok(parsed) => parsed,
        Err(err) => {
            found
                .skipped
                .push(("(file)".to_string(), format!("unreadable TOML: {err}")));
            return found;
        }
    };
    let Some(map) = parsed.get("mcp_servers").and_then(|value| value.as_table()) else {
        return found;
    };
    for (name, entry) in map {
        // Same scalar-stringify policy as the JSON side: TOML legally
        // allows integer/boolean values, and dropping one silently loses a
        // token or shifts positional args.
        let scalar = |value: &toml::Value| -> Option<String> {
            match value {
                toml::Value::String(s) => Some(s.clone()),
                toml::Value::Integer(n) => Some(n.to_string()),
                toml::Value::Float(f) => Some(f.to_string()),
                toml::Value::Boolean(b) => Some(b.to_string()),
                _ => None,
            }
        };
        let strings = |field: &str| {
            entry
                .get(field)
                .and_then(|value| value.as_table())
                .map(|table| {
                    table
                        .iter()
                        .filter_map(|(key, value)| scalar(value).map(|value| (key.clone(), value)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let args = entry
            .get("args")
            .and_then(|value| value.as_array())
            .map(|args| {
                // Same positional-shift hazard as the JSON path: inline
                // table elements must not be dropped, only stringified.
                args.iter()
                    .map(|value| scalar(value).unwrap_or_else(|| value.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let candidate = ImportedServer {
            name: name.clone(),
            command: entry
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            args,
            env: strings("env"),
            url: entry
                .get("url")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            transport: entry
                .get("transport")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            // Codex spells the headers `http_headers` and a bearer token as
            // the env var that holds it (`bearer_token_env_var`).
            headers: {
                let mut headers = strings("headers");
                headers.extend(strings("http_headers"));
                let has_authorization = headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
                if !has_authorization {
                    let token = entry.get("bearer_token").and_then(scalar).or_else(|| {
                        entry
                            .get("bearer_token_env_var")
                            .and_then(scalar)
                            .and_then(|name| std::env::var(&name).ok())
                    });
                    if let Some(token) = token {
                        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
                    }
                }
                headers
            },
        };
        if let Some(server) = usable(candidate, &mut found.skipped) {
            found.servers.push(server);
        }
    }
    found
}

/// Scan one source for importable MCP servers.
#[must_use]
pub fn scan(source: Source, home: Option<&Path>, workspace: &Path) -> Found {
    let mut total = Found::default();
    for path in source.mcp_paths(home, workspace) {
        // Only a missing file is silent: an existing-but-unreadable config
        // (permissions, non-UTF-8) recorded as "no MCP config found" would
        // read as "nothing to migrate" and invite deleting the source.
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                total
                    .skipped
                    .push((path.display().to_string(), format!("unreadable: {err}")));
                continue;
            }
        };
        let mut found = if path.extension().is_some_and(|ext| ext == "toml") {
            servers_from_toml(&raw)
        } else {
            match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(document) => servers_from_json(&document),
                Err(err) => {
                    total
                        .skipped
                        .push((path.display().to_string(), format!("unreadable: {err}")));
                    continue;
                }
            }
        };
        // `servers_from_toml` records an unparseable document under the
        // synthetic "(file)" name; name the actual path like the JSON branch
        // and do not report the file as read.
        if found
            .skipped
            .first()
            .is_some_and(|(name, _)| name == "(file)")
        {
            let (_, reason) = found.skipped.remove(0);
            total.skipped.push((path.display().to_string(), reason));
            continue;
        }
        total.read_files.push(path);
        total.servers.extend(found.servers);
        total.skipped.extend(found.skipped);
    }
    total
}

/// `/import` — report, and with `--yes` write.
pub fn run(state: &ReplState, arg: &str, yes: bool) {
    let words: Vec<String> = arg
        .split_whitespace()
        .map(|word| word.to_ascii_lowercase())
        .collect();
    let sources: Vec<Source> = words
        .iter()
        .filter(|word| !word.starts_with("--"))
        .filter_map(|word| {
            let source = Source::parse(word);
            if source.is_none() {
                println!(
                    "{} unknown source {word:?} — claude|cursor|codex|gemini",
                    "✗".red()
                );
            }
            source
        })
        .collect();
    // The all-sources default applies only when the user gave NO positional
    // words: with a typo'd source (`/import claudee --yes`) every word was
    // rejected, and falling through to "all sources" scanned and — with
    // `--yes` — wrote every found server after the error.
    let any_positional = words.iter().any(|word| !word.starts_with("--"));
    let sources = if any_positional {
        if sources.is_empty() {
            return; // every source word was rejected — do not scan everything
        }
        sources
    } else {
        Source::all().to_vec()
    };

    let home = crate::config::effective_home_dir();
    println!("{}", "importing from other agent CLIs".bold());
    let mut candidates: Vec<ImportedServer> = Vec::new();
    for source in sources {
        let found = scan(source, home.as_deref(), &state.workspace);
        println!();
        println!("{}", source.label().bold());
        for path in &found.read_files {
            println!("  read {}", path.display());
        }
        if found.read_files.is_empty() {
            println!("  {}", "no MCP config found".dimmed());
        }
        for server in &found.servers {
            println!("  + mcp server {} → {}", server.name, server.endpoint());
        }
        for (name, reason) in &found.skipped {
            println!("  {} skipped: {reason}", name.dimmed());
        }
        for note in source.extra_notes(home.as_deref()) {
            println!("  {}", note.dimmed());
        }
        candidates.extend(found.servers);
    }

    if candidates.is_empty() {
        println!();
        println!(
            "{}",
            "nothing to import — instruction files, commands, and skills are read natively; only MCP servers need copying"
                .dimmed()
        );
        return;
    }

    let config_path = state.mcp_config_path.clone();
    // `load_config` returns `Ok(default)` only for a MISSING file; the Err
    // arm means an existing mcp.json that is unreadable/unparseable. With
    // `--yes` the later save would then overwrite it with only the
    // imported servers — silently destroying the user's existing config.
    let mut config = match codesmith_agent_runtime::mcp::load_config(&config_path) {
        Ok(config) => config,
        Err(err) => {
            println!(
                "{} could not read existing MCP config {}: {err}",
                "✗".red(),
                config_path.display()
            );
            return;
        }
    };
    let mut added = Vec::new();
    // Names inserted during THIS run: two sources can define the same name
    // (e.g. Claude and Cursor both defining `context7`). The pre-existing
    // check alone misreports the second as "already configured", and in a
    // dry run (nothing inserted) both duplicates look importable while the
    // real run would add only one — this makes dry run and write agree.
    let mut seen_this_run: std::collections::HashSet<String> = std::collections::HashSet::new();
    for server in candidates {
        if config.servers.contains_key(&server.name) {
            println!(
                "  {}",
                format!("{} already configured — left alone", server.name).dimmed()
            );
            continue;
        }
        if !seen_this_run.insert(server.name.clone()) {
            println!(
                "  {}",
                format!(
                    "{} already imported from another source — left alone",
                    server.name
                )
                .dimmed()
            );
            continue;
        }
        if !yes {
            continue;
        }
        match serde_json::from_value::<codesmith_agent_runtime::mcp::McpServerConfig>(
            server.to_config_value(),
        ) {
            Ok(entry) => {
                config.servers.insert(server.name.clone(), entry);
                added.push(server.name);
            }
            Err(err) => println!(
                "{} {}: could not be converted ({err})",
                "✗".red(),
                server.name
            ),
        }
    }

    if !yes {
        println!();
        println!(
            "{}",
            format!(
                "nothing written — re-run with --yes to add them to {}",
                config_path.display()
            )
            .bold()
        );
        return;
    }
    if added.is_empty() {
        println!("{}", "nothing new to add".dimmed());
        return;
    }
    match codesmith_agent_runtime::mcp::save_config(&config_path, &config) {
        Ok(()) => println!(
            "{}",
            format!(
                "wrote {} server(s) to {} — `/mcp list` shows them; connections start with the next session",
                added.len(),
                config_path.display()
            )
            .green()
        ),
        Err(err) => println!("{} could not write the MCP config: {err}", "✗".red()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::test_support::{EnvVarGuard, lock_test_env};

    /// Redirect HOME (`/import` resolves source configs from the real home
    /// via `effective_home_dir`), CODESMITH_HOME, and CODESMITH_CONFIG_PATH
    /// into a scratch dir. Every test that constructs a `ReplState` or scans
    /// from a home must hold the env lock for the whole test.
    struct ScratchEnv {
        _home: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        _home_guard: EnvVarGuard,
        _smith_home_guard: EnvVarGuard,
        _config_guard: EnvVarGuard,
    }

    fn scratch_env() -> ScratchEnv {
        let lock = lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let codesmith_home = home.path().join(".codesmith");
        std::fs::create_dir_all(&codesmith_home).expect("create codesmith home");
        let home_guard = EnvVarGuard::set("HOME", home.path());
        let smith_home_guard = EnvVarGuard::set("CODESMITH_HOME", &codesmith_home);
        let config_guard =
            EnvVarGuard::set("CODESMITH_CONFIG_PATH", codesmith_home.join("config.toml"));
        ScratchEnv {
            _home: home,
            _lock: lock,
            _home_guard: home_guard,
            _smith_home_guard: smith_home_guard,
            _config_guard: config_guard,
        }
    }

    fn test_repl_state(mcp_config_path: std::path::PathBuf) -> ReplState {
        let workspace = tempfile::tempdir().expect("tempdir");
        let options = crate::app_state::InteractiveOptions {
            load_project_presets: false,
            model: "test-model".to_string(),
            workspace: workspace.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: workspace.path().join("skills"),
            memory_path: workspace.path().join("memory.md"),
            mcp_config_path,
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        ReplState::new(options, &crate::config::Config::default())
    }

    fn write(home: &Path, rel: &str, body: &str) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write source config");
    }

    fn claude_home_json(server_name: &str, command: &str) -> String {
        let mut servers = serde_json::Map::new();
        servers.insert(
            server_name.to_string(),
            serde_json::json!({"command": command, "args": ["server.js"], "env": {"TOKEN": "x"}}),
        );
        serde_json::json!({ "mcpServers": servers }).to_string()
    }

    #[test]
    fn json_servers_import_stdio_and_url_alike() {
        let document = serde_json::json!({
            "mcpServers": {
                "stdio": {"command": "node", "args": ["server.js"], "env": {"TOKEN": "x"}},
                "remote": {"url": "https://example.test/mcp", "headers": {"Authorization": "Bearer t"}},
                "broken": {}
            }
        });
        let found = servers_from_json(&document);
        assert_eq!(found.servers.len(), 2);
        let stdio = &found.servers[0];
        assert_eq!(stdio.name, "stdio");
        assert_eq!(stdio.command.as_deref(), Some("node"));
        assert_eq!(stdio.args, vec!["server.js".to_string()]);
        assert_eq!(stdio.env, vec![("TOKEN".to_string(), "x".to_string())]);
        let remote = &found.servers[1];
        assert_eq!(remote.name, "remote");
        assert_eq!(remote.command, None);
        assert_eq!(remote.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(
            remote.headers,
            vec![("Authorization".to_string(), "Bearer t".to_string())]
        );
        assert_eq!(
            found.skipped,
            vec![("broken".to_string(), "neither command nor url".to_string())]
        );
    }

    /// The extractor also accepts the `servers` key and Claude Code's
    /// project-scoped nesting in `~/.claude.json` (where most servers live).
    #[test]
    fn json_servers_accept_the_alias_key_and_project_nesting() {
        let document = serde_json::json!({
            "servers": {"aliased": {"command": "uvx"}},
            "projects": {
                "/some/project": {"mcpServers": {"nested": {"url": "https://example.test/n"}}},
                "/other/project": {"servers": {"also-nested": {"command": "node"}}}
            }
        });
        let found = servers_from_json(&document);
        let names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"aliased"), "{names:?}");
        assert!(names.contains(&"nested"), "{names:?}");
        assert!(names.contains(&"also-nested"), "{names:?}");
        assert!(found.skipped.is_empty());
    }

    /// Non-string scalars in env/headers/args are stringified, never
    /// dropped: a numeric token or a JSON null arg must survive with its
    /// position.
    #[test]
    fn json_import_stringifies_non_string_scalars_and_keeps_arg_positions() {
        let document = serde_json::json!({
            "mcpServers": {
                "stdio": {
                    "command": "node",
                    "args": ["--flag", null, 5, true, {"k": "v"}],
                    "env": {"TOKEN": 12345, "VERBOSE": false},
                    "headers": {"X-Count": 3}
                }
            }
        });
        let found = servers_from_json(&document);
        let server = &found.servers[0];
        assert_eq!(
            server.args,
            vec![
                "--flag".to_string(),
                "null".to_string(),
                "5".to_string(),
                "true".to_string(),
                "{\"k\":\"v\"}".to_string()
            ],
            "arg positions carry meaning; nothing may be dropped"
        );
        assert!(
            server
                .env
                .contains(&("TOKEN".to_string(), "12345".to_string()))
        );
        assert!(
            server
                .env
                .contains(&("VERBOSE".to_string(), "false".to_string()))
        );
        assert!(
            server
                .headers
                .contains(&("X-Count".to_string(), "3".to_string()))
        );
    }

    #[test]
    fn an_imported_server_becomes_a_config_entry() {
        let server = ImportedServer {
            name: "remote".to_string(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            url: Some("https://example.test/mcp".to_string()),
            transport: Some("sse".to_string()),
            headers: Vec::new(),
        };
        let entry: codesmith_agent_runtime::mcp::McpServerConfig =
            serde_json::from_value(server.to_config_value()).expect("config entry");
        assert_eq!(entry.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(entry.transport.as_deref(), Some("sse"));
        assert!(entry.enabled, "serde's default keeps a new server enabled");
    }

    /// `to_config_value` for a stdio server emits command/args/env and
    /// omits the URL-only fields; `endpoint` names how the server is
    /// reached, "(none)" included.
    #[test]
    fn stdio_config_values_and_endpoints_render() {
        let server = ImportedServer {
            name: "stdio".to_string(),
            command: Some("node".to_string()),
            args: vec!["server.js".to_string()],
            env: vec![("TOKEN".to_string(), "x".to_string())],
            url: None,
            transport: None,
            headers: Vec::new(),
        };
        assert_eq!(server.endpoint(), "node");
        let value = server.to_config_value();
        assert_eq!(value["command"], serde_json::json!("node"));
        assert_eq!(value["args"], serde_json::json!(["server.js"]));
        assert_eq!(value["env"], serde_json::json!({"TOKEN": "x"}));
        assert!(value.get("url").is_none());
        assert!(value.get("transport").is_none());

        let bare = ImportedServer {
            name: "bare".to_string(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            transport: None,
            headers: Vec::new(),
        };
        assert_eq!(bare.endpoint(), "(none)");
    }

    #[test]
    fn codex_toml_servers_import() {
        let found = servers_from_toml(
            r#"
[mcp_servers.docs]
command = "uvx"
args = ["mcp-server-fetch"]

[mcp_servers.empty]
"#,
        );
        assert_eq!(found.servers.len(), 1);
        assert_eq!(found.servers[0].name, "docs");
        assert_eq!(found.servers[0].command.as_deref(), Some("uvx"));
        assert_eq!(found.servers[0].args, vec!["mcp-server-fetch".to_string()]);
        assert_eq!(
            found.skipped,
            vec![("empty".to_string(), "neither command nor url".to_string())]
        );
    }

    /// Gemini CLI spells a remote server's endpoint `httpUrl`: it is a URL
    /// server, not "neither command nor url".
    #[test]
    fn gemini_remote_servers_come_over_by_http_url() {
        let found = servers_from_json(&serde_json::json!({
            "mcpServers": {
                "github": {"httpUrl": "https://example.test/mcp", "headers": {"X-A": "1"}}
            }
        }));
        assert!(found.skipped.is_empty(), "{:?}", found.skipped);
        assert_eq!(found.servers.len(), 1);
        assert_eq!(
            found.servers[0].url.as_deref(),
            Some("https://example.test/mcp")
        );
        assert_eq!(
            found.servers[0].headers,
            vec![("X-A".to_string(), "1".to_string())]
        );
    }

    /// Codex names the headers `http_headers` and a bearer token by the env
    /// var holding it: without them the server is imported unauthenticated.
    #[test]
    fn codex_http_headers_and_bearer_token_come_over() {
        let _lock = codesmith_agent_runtime::test_support::lock_test_env();
        let _token = codesmith_agent_runtime::test_support::EnvVarGuard::set(
            "CODESMITH_IMPORT_TEST_TOKEN",
            "s3cret",
        );
        let found = servers_from_toml(
            r#"
[mcp_servers.remote]
url = "https://example.test/mcp"
http_headers = { "X-Thing" = "value" }
bearer_token_env_var = "CODESMITH_IMPORT_TEST_TOKEN"
"#,
        );
        assert_eq!(found.servers.len(), 1);
        let headers = &found.servers[0].headers;
        assert!(
            headers.contains(&("X-Thing".to_string(), "value".to_string())),
            "{headers:?}"
        );
        assert!(
            headers.contains(&("Authorization".to_string(), "Bearer s3cret".to_string())),
            "{headers:?}"
        );
    }

    /// The TOML side stringifies legal non-string scalars with the same
    /// policy as the JSON side.
    #[test]
    fn codex_toml_stringifies_non_string_scalars() {
        let found = servers_from_toml(
            r#"
[mcp_servers.docs]
command = "uvx"
args = ["--port", 8080, true]
env = { TOKEN = 12345, VERBOSE = false, RATIO = 1.5 }
"#,
        );
        assert_eq!(found.servers.len(), 1);
        let server = &found.servers[0];
        assert_eq!(
            server.args,
            vec!["--port".to_string(), "8080".to_string(), "true".to_string()]
        );
        assert!(
            server
                .env
                .contains(&("TOKEN".to_string(), "12345".to_string()))
        );
        assert!(
            server
                .env
                .contains(&("VERBOSE".to_string(), "false".to_string()))
        );
        assert!(
            server
                .env
                .contains(&("RATIO".to_string(), "1.5".to_string()))
        );
    }

    #[test]
    fn an_unparseable_toml_names_the_file_and_is_not_reported_as_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "mcp_servers = ").unwrap();
        let found = scan(Source::Codex, None, dir.path());
        assert!(found.read_files.is_empty(), "the file did not parse");
        assert_eq!(found.skipped.len(), 1);
        assert!(
            found.skipped[0].0.contains("config.toml"),
            "skip names the failing path: {:?}",
            found.skipped[0]
        );
        assert!(
            found.skipped[0].1.starts_with("unreadable TOML"),
            "skip reason: {:?}",
            found.skipped[0]
        );
    }

    /// `scan` walks each source's home and workspace paths: present files
    /// are read, missing ones are silent, and an existing-but-unparseable
    /// JSON config is reported as unreadable (never as "no MCP config").
    #[test]
    fn scan_reads_home_and_workspace_paths_and_flags_bad_json() {
        let _env = scratch_env();
        let home = tempfile::tempdir().expect("tempdir");
        let workspace = tempfile::tempdir().expect("tempdir");

        write(
            home.path(),
            ".claude.json",
            &claude_home_json("home-server", "node"),
        );
        write(
            workspace.path(),
            ".mcp.json",
            &serde_json::json!({"mcpServers": {"ws-server": {"url": "https://example.test/w"}}})
                .to_string(),
        );

        let found = scan(Source::Claude, Some(home.path()), workspace.path());
        let names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["home-server", "ws-server"]);
        assert_eq!(found.read_files.len(), 2, "both configs were read");
        assert!(found.skipped.is_empty());

        // An existing-but-broken config is a skip with the path, not silence.
        std::fs::write(workspace.path().join(".mcp.json"), "{not json").unwrap();
        let found = scan(Source::Claude, Some(home.path()), workspace.path());
        assert_eq!(found.servers.len(), 1, "only the home server survives");
        assert_eq!(found.skipped.len(), 1, "{:?}", found.skipped);
        assert!(found.skipped[0].0.ends_with(".mcp.json"));
        assert!(
            found.skipped[0].1.starts_with("unreadable"),
            "{:?}",
            found.skipped
        );
        assert_eq!(found.read_files.len(), 1, "the broken file is not 'read'");
    }

    /// The Codex scan path parses `.codex/config.toml` from both home and
    /// workspace.
    #[test]
    fn scan_reads_codex_toml_from_home_and_workspace() {
        let home = tempfile::tempdir().expect("tempdir");
        let workspace = tempfile::tempdir().expect("tempdir");
        let toml = "[mcp_servers.docs]\ncommand = \"uvx\"\n";
        write(home.path(), ".codex/config.toml", toml);
        write(
            workspace.path(),
            ".codex/config.toml",
            "[mcp_servers.ws]\nurl = \"https://example.test/t\"\n",
        );

        let found = scan(Source::Codex, Some(home.path()), workspace.path());
        let names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "ws"]);
    }

    #[test]
    fn source_words_parse() {
        assert_eq!(Source::parse("claude"), Some(Source::Claude));
        assert_eq!(Source::parse("Claude-Code"), Some(Source::Claude));
        assert_eq!(Source::parse("cursor"), Some(Source::Cursor));
        assert_eq!(Source::parse("codex"), Some(Source::Codex));
        assert_eq!(Source::parse("gemini"), Some(Source::Gemini));
        assert_eq!(Source::parse("windsurf"), None);
    }

    #[test]
    fn source_aliases_parse_and_labels_render() {
        assert_eq!(Source::parse(" CC "), Some(Source::Claude));
        assert_eq!(Source::parse("openai-codex"), Some(Source::Codex));
        assert_eq!(Source::parse("gemini-cli"), Some(Source::Gemini));
        assert_eq!(
            Source::all(),
            [
                Source::Claude,
                Source::Cursor,
                Source::Codex,
                Source::Gemini
            ]
        );
        assert_eq!(Source::Claude.label(), "claude");
        assert_eq!(Source::Cursor.label(), "cursor");
        assert_eq!(Source::Codex.label(), "codex");
        assert_eq!(Source::Gemini.label(), "gemini");
    }

    /// `/import claude` without `--yes` is a report: nothing is written and
    /// no config file appears.
    #[test]
    fn run_dry_run_reports_without_writing() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("dry-run-server", "node"),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claude", false);

        assert!(
            !mcp_path.exists(),
            "a dry run must not create the MCP config"
        );
    }

    /// `/import claude --yes` writes the found server into the configured
    /// `mcp.json`.
    #[test]
    fn run_with_yes_writes_the_found_servers() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("written-server", "node"),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claude", true);

        let body = std::fs::read_to_string(&mcp_path).expect("mcp.json written");
        let config: serde_json::Value = serde_json::from_str(&body).expect("mcp.json parses");
        let server = &config["servers"]["written-server"];
        assert_eq!(server["command"], serde_json::json!("node"));
        assert_eq!(server["args"], serde_json::json!(["server.js"]));
        assert_eq!(server["env"], serde_json::json!({"TOKEN": "x"}));
        assert_eq!(
            server["enabled"],
            serde_json::json!(true),
            "entries land in this client's full shape"
        );
    }

    /// A name already present in the target config is left alone — the
    /// existing entry wins.
    #[test]
    fn run_leaves_pre_configured_names_alone() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("clash", "imported-command"),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        std::fs::write(
            &mcp_path,
            serde_json::json!({"servers": {"clash": {"command": "keep-me"}}}).to_string(),
        )
        .unwrap();
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claude", true);

        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            config["servers"]["clash"]["command"],
            serde_json::json!("keep-me"),
            "the pre-existing entry wins"
        );
        assert_eq!(
            config["servers"].as_object().unwrap().len(),
            1,
            "nothing else was added"
        );
    }

    /// Two sources defining the same name import it once (dry run and write
    /// agree on which one).
    #[test]
    fn run_deduplicates_a_name_shared_by_two_sources() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("shared", "claude-command"),
        );
        write(
            &home,
            ".cursor/mcp.json",
            &serde_json::json!({"mcpServers": {"shared": {"command": "cursor-command"}}})
                .to_string(),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claude cursor", true);

        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let servers = config["servers"].as_object().expect("servers");
        assert_eq!(servers.len(), 1, "the name imports exactly once");
        assert_eq!(
            servers["shared"]["command"],
            serde_json::json!("claude-command"),
            "the first scanned source wins"
        );
    }

    /// A typo'd source word with `--yes` must not fall through to "all
    /// sources" and write everything it finds.
    #[test]
    fn run_with_only_rejected_source_words_scans_nothing() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("would-be-imported", "node"),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claudee --yes", true);

        assert!(
            !mcp_path.exists(),
            "every rejected source word must not scan (let alone write) anything"
        );
    }

    /// No positional word scans every source; with nothing found the
    /// command reports and writes nothing (also covers each source's
    /// extra-notes surface on a present home).
    #[test]
    fn run_without_a_source_word_scans_every_source_and_finds_nothing() {
        let _env = scratch_env();
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        let state = test_repl_state(mcp_path.clone());

        run(&state, "--dry-run", false);

        assert!(
            !mcp_path.exists(),
            "nothing was found, so nothing may be written"
        );
    }

    /// An existing `mcp.json` that cannot be parsed is never overwritten —
    /// with `--yes` the save would otherwise destroy the user's config.
    #[test]
    fn run_refuses_to_clobber_an_unparseable_config() {
        let _env = scratch_env();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        write(
            &home,
            ".claude.json",
            &claude_home_json("clobber-attempt", "node"),
        );
        let out = tempfile::tempdir().expect("tempdir");
        let mcp_path = out.path().join("mcp.json");
        std::fs::write(&mcp_path, "{definitely not json").unwrap();
        let state = test_repl_state(mcp_path.clone());

        run(&state, "claude", true);

        assert_eq!(
            std::fs::read_to_string(&mcp_path).unwrap(),
            "{definitely not json",
            "the unreadable config must survive a --yes run byte-for-byte"
        );
    }
}
