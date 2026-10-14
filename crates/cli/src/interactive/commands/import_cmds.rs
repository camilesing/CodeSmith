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
//!   `url`/`transport`/`headers`). An entry with neither is skipped.
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
        .map(|args| args.iter().filter_map(scalar).collect())
        .unwrap_or_default();
    ImportedServer {
        name: name.to_string(),
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
            .map(|args| args.iter().filter_map(scalar).collect())
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
            headers: strings("headers"),
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
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
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
    let sources = if sources.is_empty() {
        Source::all().to_vec()
    } else {
        sources
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
    let mut config = match crate::mcp::load_config(&config_path) {
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
        match serde_json::from_value::<crate::mcp::McpServerConfig>(server.to_config_value()) {
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
    match crate::mcp::save_config(&config_path, &config) {
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
        let entry: crate::mcp::McpServerConfig =
            serde_json::from_value(server.to_config_value()).expect("config entry");
        assert_eq!(entry.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(entry.transport.as_deref(), Some("sse"));
        assert!(entry.enabled, "serde's default keeps a new server enabled");
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

    #[test]
    fn source_words_parse() {
        assert_eq!(Source::parse("claude"), Some(Source::Claude));
        assert_eq!(Source::parse("Claude-Code"), Some(Source::Claude));
        assert_eq!(Source::parse("cursor"), Some(Source::Cursor));
        assert_eq!(Source::parse("codex"), Some(Source::Codex));
        assert_eq!(Source::parse("gemini"), Some(Source::Gemini));
        assert_eq!(Source::parse("windsurf"), None);
    }
}
