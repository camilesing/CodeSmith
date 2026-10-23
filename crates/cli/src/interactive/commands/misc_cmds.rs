//! Remaining REPL commands — ports of the smaller `commands/` modules
//! plus honest degradation notes for composer-bound features (queue,
//! stash) that have no line-mode equivalent yet.

use colored::Colorize;

use super::ReplState;
use crate::app_state::HuntVerdict;
use crate::config::ApiProvider;

/// `/init` — write/update AGENTS.md (reuses the legacy generator).
pub fn init(state: &ReplState) {
    let workspace = &state.workspace;
    super::project_doc::ensure_codesmith_gitignored(workspace);
    let agents_path = workspace.join("AGENTS.md");
    let already_exists = agents_path.exists();
    let content = super::project_doc::generate_project_doc(workspace);
    match std::fs::write(&agents_path, content) {
        Ok(()) => {
            let verb = if already_exists { "Updated" } else { "Created" };
            println!(
                "{verb} AGENTS.md at {} — edit it to customize agent behavior for this project.",
                agents_path.display()
            );
        }
        Err(e) => println!("{} failed to write AGENTS.md: {e}", "✗".red()),
    }
}

/// `/hooks [list|events]`
pub fn hooks(state: &ReplState, arg: Option<&str>) {
    let sub = arg.map(str::trim).unwrap_or("list").to_ascii_lowercase();
    match sub.as_str() {
        "" | "list" | "ls" | "show" => {
            let config = state.hooks.config();
            if config.hooks.is_empty() {
                println!(
                    "{}",
                    "no hooks configured — add a [[hooks.hooks]] entry to ~/.codesmith/config.toml"
                        .dimmed()
                );
                return;
            }
            println!(
                "hooks: {} ({})",
                config.hooks.len(),
                if config.enabled {
                    "enabled"
                } else {
                    "globally disabled"
                }
            );
            for hook in &config.hooks {
                println!("  • on {:?}: {}", hook.event, hook.command);
            }
        }
        "events" | "event" | "list-events" => {
            println!(
                "{}",
                "hook events: session_start, session_end, message_submit, tool_call_before, tool_call_after, mode_change, on_error, shell_env, task_created, task_completed, pre_compact, turn_end, subagent_spawn, subagent_complete".dimmed()
            );
        }
        other => println!(
            "{} unknown subcommand `{other}` — try /hooks list or /hooks events",
            "✗".red()
        ),
    }
}

/// `/mcp …` — delegates to the same implementation as `codesmith mcp`.
pub async fn mcp(state: &mut ReplState, args: &str) {
    use crate::main_root::McpCommand;

    let raw = args.trim();
    let command = if raw.is_empty()
        || raw.eq_ignore_ascii_case("status")
        || raw.eq_ignore_ascii_case("list")
    {
        McpCommand::List
    } else {
        let mut parts = raw.split_whitespace();
        let action = parts.next().unwrap_or_default().to_ascii_lowercase();
        let rest: Vec<&str> = parts.collect();
        match action.as_str() {
            "init" => McpCommand::Init {
                force: rest.iter().any(|p| *p == "--force" || *p == "-f"),
            },
            "connect" | "status" | "list" => McpCommand::Connect {
                server: rest.first().map(|s| (*s).to_string()),
            },
            "tools" => McpCommand::Tools {
                server: rest.first().map(|s| (*s).to_string()),
            },
            "auth" => match rest.first() {
                Some(server) => McpCommand::Auth {
                    server: server.to_string(),
                },
                None => {
                    println!("{} usage: /mcp auth <server>", "✗".red());
                    return;
                }
            },
            "add" => {
                // Accept both the slash form (`/mcp add stdio name cmd…`)
                // and the flag form the CLI subcommand uses.
                if rest.len() >= 3 && matches!(rest[0], "stdio") {
                    McpCommand::Add {
                        name: rest[1].to_string(),
                        command: Some(rest[2].to_string()),
                        url: None,
                        transport: None,
                        args: rest[3..].iter().map(|s| s.to_string()).collect(),
                    }
                } else if rest.len() >= 3 {
                    McpCommand::Add {
                        name: rest[1].to_string(),
                        command: None,
                        url: Some(rest[2].to_string()),
                        transport: if rest[0] == "http" {
                            None
                        } else {
                            Some(rest[0].to_string())
                        },
                        args: Vec::new(),
                    }
                } else {
                    println!(
                        "{} usage: /mcp add stdio <name> <command> [args…] | /mcp add <http|sse|ws…> <name> <url>",
                        "✗".red()
                    );
                    return;
                }
            }
            "enable" => match rest.first() {
                Some(name) => McpCommand::Enable {
                    name: name.to_string(),
                },
                None => {
                    println!("{} usage: /mcp enable <name>", "✗".red());
                    return;
                }
            },
            "disable" => match rest.first() {
                Some(name) => McpCommand::Disable {
                    name: name.to_string(),
                },
                None => {
                    println!("{} usage: /mcp disable <name>", "✗".red());
                    return;
                }
            },
            "remove" | "rm" => match rest.first() {
                Some(name) => McpCommand::Remove {
                    name: name.to_string(),
                },
                None => {
                    println!("{} usage: /mcp remove <name>", "✗".red());
                    return;
                }
            },
            "validate" => McpCommand::Validate,
            "reload" | "reconnect" => McpCommand::Connect { server: None },
            _ => {
                println!(
                    "{} usage: /mcp [list|init|add|enable|disable|remove|tools|validate|reload|auth]",
                    "✗".red()
                );
                return;
            }
        }
    };
    if let Err(err) = crate::main_root::run_mcp_command(&state.config.clone(), command).await {
        println!("{} {err}", "✗".red());
    }
    state.mcp_configured_count = codesmith_agent_runtime::mcp::load_config(&state.mcp_config_path)
        .map(|cfg| cfg.servers.len())
        .unwrap_or(0);
}

/// `/network [list|allow|host>|deny <host>|remove <host>|default <policy>]`
pub fn network(arg: Option<&str>) {
    match super::network_store::network_inner(arg) {
        Ok(message) => println!("{message}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// `/goal` / `/hunt`
pub async fn goal(state: &mut ReplState, arg: Option<&str>) {
    match arg.map(str::trim) {
        Some("clear") | Some("reset") => {
            state.hunt.quarry = None;
            state.hunt.token_budget = None;
            state.hunt.started_at = None;
            state.hunt.verdict = HuntVerdict::default();
            println!("hunt cleared.");
        }
        Some("done") | Some("complete") | Some("hunted") => {
            state.hunt.verdict = HuntVerdict::Hunted;
            println!("{}", "hunt closed: HUNTED".green());
        }
        Some("wound") | Some("wounded") => {
            state.hunt.verdict = HuntVerdict::Wounded;
            println!("hunt closed: WOUNDED");
        }
        Some("escape") | Some("escaped") => {
            state.hunt.verdict = HuntVerdict::Escaped;
            println!("hunt closed: ESCAPED");
        }
        Some(text) if !text.is_empty() => {
            let (objective, budget) = parse_hunt_budget(text);
            if objective.is_empty() || objective.chars().all(|c| c == '|') {
                println!("{} usage: /hunt <quarry> [budget: N]", "✗".red());
                return;
            }
            state.hunt.quarry = Some(objective.clone());
            state.hunt.token_budget = budget;
            state.hunt.started_at = Some(std::time::Instant::now());
            state.hunt.verdict = HuntVerdict::Hunting;
            let budget_str = budget
                .map(|b| format!(" (budget: {b} tokens)"))
                .unwrap_or_default();
            println!("hunt set: \"{objective}\"{budget_str} — tracking progress.");
            let _ = super::super::submit(state, objective).await;
        }
        _ => {
            if let Some(quarry) = state.hunt.quarry.as_ref() {
                let budget_str = state
                    .hunt
                    .token_budget
                    .map(|b| {
                        let used = state.session.total_conversation_tokens;
                        let pct = if b > 0 {
                            (used as f64 / b as f64 * 100.0).min(100.0)
                        } else {
                            0.0
                        };
                        format!(" | tokens: {used}/{b} ({pct:.0}%)")
                    })
                    .unwrap_or_default();
                let verdict = match state.hunt.verdict {
                    HuntVerdict::Hunting => "[HUNTING]",
                    HuntVerdict::Hunted => "[HUNTED]",
                    HuntVerdict::Wounded => "[WOUNDED]",
                    HuntVerdict::Escaped => "[ESCAPED]",
                };
                println!("hunt {verdict}: \"{quarry}\"{budget_str}");
            } else {
                println!("no active hunt — /hunt <quarry> [budget: N] to set one");
            }
        }
    }
}

/// `[budget: N]` suffix parser (port of commands/goal.rs).
fn parse_hunt_budget(text: &str) -> (String, Option<u32>) {
    const MARKER: &str = "budget:";
    if let Some(index) = text.to_ascii_lowercase().rfind(MARKER) {
        let head = text[..index].trim_end();
        let tail = &text[index + MARKER.len()..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(budget) = digits.parse::<u32>() {
            return (head.trim().to_string(), Some(budget));
        }
    }
    (text.trim().to_string(), None)
}

/// `/task [add <prompt>|list|show <id>|cancel <id>]`
pub async fn task(state: &mut ReplState, args: &str) {
    use crate::task_manager::NewTaskRequest;

    let raw = args.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") {
        let Some(manager) = state.task_manager.clone() else {
            println!("{} task manager unavailable", "✗".red());
            return;
        };
        let tasks = manager.list_tasks(Some(30)).await;
        if tasks.is_empty() {
            println!("no tasks");
            return;
        }
        for task in tasks {
            println!("  {} [{:?}] {}", task.id, task.status, task.prompt_summary);
        }
        return;
    }
    let mut parts = raw.splitn(2, char::is_whitespace);
    let action = parts.next().unwrap_or_default().to_ascii_lowercase();
    let remainder = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match action.as_str() {
        "add" => {
            let Some(prompt) = remainder else {
                println!("{} usage: /task add <prompt>", "✗".red());
                return;
            };
            let Some(manager) = state.task_manager.clone() else {
                println!("{} task manager unavailable", "✗".red());
                return;
            };
            let request = NewTaskRequest {
                prompt: prompt.to_string(),
                model: Some(state.model.clone()),
                workspace: Some(state.workspace.clone()),
                mode: Some(state.mode.label().to_string()),
                allow_shell: Some(state.allow_shell),
                trust_mode: Some(state.trust_mode),
                auto_approve: Some(
                    state.approval_mode == codesmith_agent_runtime::mode::ApprovalMode::Auto,
                ),
            };
            match manager.add_task(request).await {
                Ok(task) => println!("task queued: {} ({})", task.id, task.prompt),
                Err(err) => println!("{} failed to queue task: {err}", "✗".red()),
            }
        }
        "show" => {
            let Some(id) = remainder else {
                println!("{} usage: /task show <id>", "✗".red());
                return;
            };
            let Some(manager) = state.task_manager.clone() else {
                println!("{} task manager unavailable", "✗".red());
                return;
            };
            match manager.get_task(id).await {
                Ok(task) => println!("{task:#?}"),
                Err(err) => println!("{} task lookup failed: {err}", "✗".red()),
            }
        }
        "cancel" | "stop" => {
            let Some(id) = remainder else {
                println!("{} usage: /task cancel <id>", "✗".red());
                return;
            };
            let Some(manager) = state.task_manager.clone() else {
                println!("{} task manager unavailable", "✗".red());
                return;
            };
            match manager.cancel_task(id).await {
                Ok(_) => println!("task {id} cancelled"),
                Err(err) => println!("{} cancel failed: {err}", "✗".red()),
            }
        }
        _ => println!(
            "{} usage: /task [add <prompt>|list|show <id>|cancel <id>]",
            "✗".red()
        ),
    }
}

/// `/jobs` — running shell jobs.
pub fn jobs(state: &ReplState) {
    let jobs = {
        let Ok(mut manager) = state.shell_manager.lock() else {
            println!("{} shell manager lock poisoned", "✗".red());
            return;
        };
        manager.list_jobs()
    };
    let running: Vec<_> = jobs
        .iter()
        .filter(|job| matches!(job.status, crate::tools::shell::ShellStatus::Running))
        .collect();
    if running.is_empty() {
        println!("no running shell jobs");
        return;
    }
    for job in running {
        println!("  {} ({}ms): {}", job.id, job.elapsed_ms, job.command);
    }
}

/// `/balance` — provider balance availability note (parity with the TUI,
/// which also defers network dispatch).
pub fn balance(state: &ReplState) {
    match state.api_provider {
        ApiProvider::Deepseek | ApiProvider::Openrouter | ApiProvider::Novita => println!(
            "balance check for {} is planned, but provider balance network dispatch is not wired in this build yet.",
            state.api_provider.display_name()
        ),
        _ => println!(
            "balance check is not supported for {} yet — check the provider dashboard.",
            state.api_provider.display_name()
        ),
    }
}

/// `/verify [stats]`
pub fn verify(arg: Option<&str>) {
    let arg = arg.map(str::trim).filter(|s| !s.is_empty());
    match arg {
        None | Some("stats") => println!("{}", verify_stats_text()),
        Some(other) => println!(
            "{} unknown subcommand '{other}' — usage: /verify [stats]",
            "✗".red()
        ),
    }
}

/// `/links` — provider dashboard/docs links.
pub fn links() {
    println!(
        "DeepSeek\n\
─────────────────────────────\n\
Dashboard https://platform.deepseek.com\n\
Docs      https://platform.deepseek.com/docs"
    );
}

/// `/home` — session overview (the TUI's home dashboard, line-mode).
pub fn home(state: &ReplState) {
    println!("{}", "session overview".bold());
    println!("============================================");
    println!("Model:     {}", state.model_display_label());
    println!("Mode:      {}", state.mode.label());
    println!("Workspace: {}", state.workspace.display());
    println!(
        "Tokens:    {} total / {} ctx window used",
        state.session.total_tokens,
        state
            .session
            .last_prompt_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into())
    );
    println!("Cost:      {:.4} usd", state.session.session_cost);
    println!("Agents:    {} running", state.running_agent_count());
    println!(
        "\n{}",
        "quick actions: /model /preset /sessions /skills /help".dimmed()
    );
}

/// `/profile <name>` — profile switching needs a config reload.
pub fn profile(state: &ReplState, arg: Option<&str>) {
    let Some(name) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
        println!(
            "current profile: {} — profiles are defined under [profiles] in ~/.codesmith/config.toml",
            state.config_profile.as_deref().unwrap_or("(default)")
        );
        return;
    };
    println!(
        "{}",
        format!("restart with the profile to switch: codesmith --profile {name}").dimmed()
    );
}

/// `/workspace [path]` — show the workspace; switching is live via `/cd`.
pub fn workspace(state: &ReplState, arg: Option<&str>) {
    let Some(raw) = arg.map(str::trim).filter(|path| !path.is_empty()) else {
        println!("current workspace: {}", state.workspace.display());
        return;
    };
    let expanded = if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        home.join(rest)
    } else {
        std::path::PathBuf::from(raw)
    };
    let candidate = if expanded.is_absolute() {
        expanded
    } else {
        state.workspace.join(expanded)
    };
    if !candidate.is_dir() {
        println!(
            "{} workspace does not exist or is not a directory: {}",
            "✗".red(),
            candidate.display()
        );
        return;
    }
    println!(
        "{}",
        format!(
            "use /cd {} to move this session there (live; restart only to load the new directory's project config and AGENTS.md)",
            candidate.display()
        )
        .dimmed()
    );
}

/// `/translate` — toggle response-locale translation.
pub fn translate(state: &mut ReplState) {
    state.translation_enabled = !state.translation_enabled;
    println!(
        "translation {} (responds in {})",
        if state.translation_enabled {
            "on"
        } else {
            "off"
        },
        state.ui_locale.tag()
    );
}

/// `/anchor <text>|list|remove <n>`
pub fn anchor(state: &ReplState, content: Option<&str>) {
    let path = state.workspace.join(".codesmith").join("anchors.md");
    let Some(input) = content.map(str::trim).filter(|s| !s.is_empty()) else {
        println!("{} usage: /anchor <text> | list | remove <n>", "✗".red());
        return;
    };
    if input.eq_ignore_ascii_case("list") {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let anchors: Vec<&str> = content
            .split("\n---\n")
            .filter(|s| !s.trim().is_empty())
            .collect();
        if anchors.is_empty() {
            println!("no anchors yet");
            return;
        }
        for (idx, anchor) in anchors.iter().enumerate() {
            println!("  {}) {}", idx + 1, anchor.trim());
        }
        return;
    }
    if let Some(rest) = input
        .strip_prefix("remove ")
        .or_else(|| input.strip_prefix("rm "))
        .or_else(|| input.strip_prefix("delete "))
    {
        let Ok(index) = rest.trim().parse::<usize>() else {
            println!("{} usage: /anchor remove <n>", "✗".red());
            return;
        };
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut anchors: Vec<String> = content
            .split("\n---\n")
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if index == 0 || index > anchors.len() {
            println!("{} no anchor #{index}", "✗".red());
            return;
        }
        anchors.remove(index - 1);
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&state.workspace));
        match codesmith_agent_runtime::utils::write_atomic(
            &path,
            anchors.join("\n---\n").as_bytes(),
        ) {
            Ok(()) => println!("removed anchor #{index}"),
            Err(err) => println!("{} failed to write anchors: {err}", "✗".red()),
        }
        return;
    }
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let entry = format!("[{timestamp}] {input}");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    if !content.is_empty() {
        content.push_str("\n---\n");
    }
    content.push_str(&entry);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&state.workspace));
    match codesmith_agent_runtime::utils::write_atomic(&path, content.as_bytes()) {
        Ok(()) => println!("anchor added: {entry}"),
        Err(err) => println!("{} failed to write anchors: {err}", "✗".red()),
    }
}

/// `/note <text>|add|list|show|remove|clear|path|help`
pub fn note(state: &ReplState, content: Option<&str>) {
    let path = state.workspace.join(".codesmith").join("notes.md");
    let Some(input) = content.map(str::trim).filter(|s| !s.is_empty()) else {
        println!(
            "{} usage: /note <text> (also: list, show <n>, remove <n>, clear, path)",
            "✗".red()
        );
        return;
    };
    let (command, rest) = match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], Some(input[index..].trim())),
        None => (input, None),
    };
    match command.to_ascii_lowercase().as_str() {
        "list" => {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let notes: Vec<&str> = content
                .split("\n---\n")
                .filter(|s| !s.trim().is_empty())
                .collect();
            if notes.is_empty() {
                println!("no notes yet");
                return;
            }
            for (idx, note) in notes.iter().enumerate() {
                let first_line = note.lines().next().unwrap_or("");
                println!("  {}) {}", idx + 1, first_line);
            }
        }
        "show" => {
            let Ok(index) = rest.unwrap_or_default().parse::<usize>() else {
                println!("{} usage: /note show <n>", "✗".red());
                return;
            };
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let notes: Vec<&str> = content
                .split("\n---\n")
                .filter(|s| !s.trim().is_empty())
                .collect();
            match notes.get(index.checked_sub(1).map_or(0, |i| i)) {
                Some(note) if index > 0 => println!("{note}"),
                _ => println!("{} no note #{index}", "✗".red()),
            }
        }
        "remove" | "rm" | "delete" => {
            let Ok(index) = rest.unwrap_or_default().parse::<usize>() else {
                println!("{} usage: /note remove <n>", "✗".red());
                return;
            };
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let mut notes: Vec<String> = content
                .split("\n---\n")
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if index == 0 || index > notes.len() {
                println!("{} no note #{index}", "✗".red());
                return;
            }
            notes.remove(index - 1);
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&state.workspace));
            let _ = codesmith_agent_runtime::utils::write_atomic(
                &path,
                notes.join("\n---\n").as_bytes(),
            );
            println!("removed note #{index}");
        }
        "clear" => {
            let _ = codesmith_agent_runtime::utils::write_atomic(&path, b"");
            println!("notes cleared: {}", path.display());
        }
        "path" => println!("notes path: {}", path.display()),
        "help" => println!("usage: /note <text> | list | show <n> | remove <n> | clear | path"),
        _ => {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
            let entry = format!("[{timestamp}] {input}");
            let mut content = std::fs::read_to_string(&path).unwrap_or_default();
            if !content.is_empty() {
                content.push_str("\n---\n");
            }
            content.push_str(&entry);
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&state.workspace));
            match codesmith_agent_runtime::utils::write_atomic(&path, content.as_bytes()) {
                Ok(()) => println!("note added"),
                Err(err) => println!("{} failed to write notes: {err}", "✗".red()),
            }
        }
    }
}

/// `/attach <image path>` — queue an image for the next message.
///
/// Admission is the metadata-level [`AttachmentStore::precheck`] (TODO-50):
/// wire-representable type (`WIRE_IMAGE_EXTENSIONS` — the request builder
/// drops anything else without a trace, so refusing here is the loud-failure
/// point; video has no wire form), per-file byte cap, and the per-message
/// count cap against what is already queued. A refusal is printed and
/// recorded on the session as an excluded context reference. The hash + copy
/// into the content-addressed store happens at send time
/// (`crate::media::ingest_for_message`), off the runtime thread.
pub fn attach(state: &mut ReplState, arg: Option<&str>) {
    use codesmith_agent_runtime::attachments::{AttachmentStore, MAX_ATTACHMENTS_PER_MESSAGE};
    use codesmith_agent_runtime::session_store::ContextReferenceSource;

    let Some(raw_path) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        println!("{} usage: /attach <image-path>", "✗".red());
        return;
    };
    let path = std::path::PathBuf::from(raw_path);
    let path = if path.is_absolute() {
        path
    } else {
        state.workspace.join(path)
    };
    let Ok(path) = path.canonicalize() else {
        println!("{} attachment not found: {}", "✗".red(), path.display());
        return;
    };
    let next_message = state.api_messages.len();
    let admitted = AttachmentStore::check_message_count(state.pending_attachments.len() + 1)
        .and_then(|()| AttachmentStore::precheck(&path));
    let (_, bytes) = match admitted {
        Ok(ok) => ok,
        Err(err) => {
            println!("{} attachment refused: {err}", "✗".red());
            state
                .context_references
                .push(crate::media::rejected_reference(
                    &path,
                    ContextReferenceSource::Attachment,
                    next_message,
                    &err.to_string(),
                ));
            return;
        }
    };
    state.pending_attachments.push(path.clone());
    println!(
        "attached: {} ({bytes} bytes; {}/{MAX_ATTACHMENTS_PER_MESSAGE} queued — stored content-addressed when the message is sent)",
        path.display(),
        state.pending_attachments.len()
    );
}

/// Claim-check stats (moved from the retired `commands/verify.rs`).
fn verify_stats_text() -> String {
    let verdicts = codesmith_agent_runtime::evolution_log::verdicts_path();
    let Some(path) = verdicts.as_deref() else {
        return "claim-check log unavailable (home directory not resolvable)".to_string();
    };
    if !path.exists() {
        return "No claim-check verdicts recorded yet — they appear when a completed \
turn's final message claims \"tests pass / build succeeds\" and the verifier \
checks it (see [verification] result_claims in config.example.toml)."
            .to_string();
    }
    let records = codesmith_agent_runtime::evolution_log::load_verdicts(path);
    if records.is_empty() {
        return format!(
            "claim-check log at {} contains no readable records",
            codesmith_agent_runtime::utils::display_path(path)
        );
    }
    format!(
        "{}\nlog: {}",
        codesmith_agent_runtime::evolution_log::summarize(&records).render(),
        codesmith_agent_runtime::utils::display_path(path)
    )
}

/// `/share` — upload conversation to a paste service.
pub async fn share(state: &mut ReplState) {
    let history_json = serde_json::to_string(&state.api_messages).unwrap_or_default();
    match super::share::perform_share(&history_json, &state.model, state.mode.label()).await {
        Ok(url) => println!("shared: {url}"),
        Err(err) => println!("{} share failed: {err}", "✗".red()),
    }
}

// === Degraded commands (composer-bound features) ===

/// The changelog as of this build.
///
/// Embedded rather than read from disk: an installed binary does not ship
/// the repository, and release notes that only work from a source checkout
/// would be a command that silently does nothing for the people who need it
/// most.
const CHANGELOG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md"));

/// `/recap` — a one-line summary of the session so far, on demand.
///
/// Claude Code also shows a recap automatically when you come back to the
/// terminal after a few minutes; that needs an idle timer and a background
/// model call per idle period, and is not wired here — this is the on-demand
/// half, which is the half a user asks for.
///
/// The summary is capped at [`RECAP_MAX_CHARS`], as Claude Code caps it: a
/// recap that needs scrolling is a compaction, and `/compact` is that.
pub async fn recap(state: &ReplState) {
    if state.api_messages.len() < 2 {
        println!(
            "{}",
            "nothing to recap yet — the session needs at least one exchange".dimmed()
        );
        return;
    }
    let digest = recent_digest(state, RECAP_SOURCE_MESSAGES);
    let prompt = format!(
        "Summarize this coding session in at most {RECAP_MAX_CHARS} characters, one or two \
         sentences, for the person who was away: what was worked on, what state it is in now, \
         and the next step if one is pending. No preamble, no bullet points.\n\n{digest}"
    );
    let messages = vec![crate::models::Message {
        role: "user".to_string(),
        content: vec![crate::models::ContentBlock::Text {
            text: prompt,
            cache_control: None,
        }],
    }];
    match crate::main_root::one_shot_messages(&state.config, &state.model, messages, None).await {
        Ok(answer) => {
            let mut text = answer.trim().to_string();
            if text.chars().count() > RECAP_MAX_CHARS {
                text = text.chars().take(RECAP_MAX_CHARS).collect::<String>() + "…";
            }
            println!("{}", text);
        }
        Err(err) => println!("{} recap failed: {err}", "✗".red()),
    }
}

/// Cap on the recap, matching Claude Code's own.
pub const RECAP_MAX_CHARS: usize = 400;

/// How many recent messages the recap reads.
const RECAP_SOURCE_MESSAGES: usize = 12;

/// A compact rendering of the newest messages, for the recap prompt.
fn recent_digest(state: &ReplState, count: usize) -> String {
    let start = state.api_messages.len().saturating_sub(count);
    let mut digest = String::new();
    for message in &state.api_messages[start..] {
        let text: String = message
            .content
            .iter()
            .filter_map(|block| match block {
                crate::models::ContentBlock::Text { text, .. } => Some(text.as_str()),
                crate::models::ContentBlock::ToolUse { name, .. } => {
                    Some(if name.is_empty() { "a tool" } else { name })
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let text: String = text.chars().take(600).collect();
        digest.push_str(&format!("{}: {text}\n", message.role));
    }
    digest
}

/// `/output-style [default|concise|status]` — the answer style.
///
/// CodeSmith has one style knob rather than Claude Code's named styles:
/// `is_simple`, the compression style the engine already refreshes the system
/// prompt for. `concise` turns it on, `default` turns it off, and the other
/// Claude Code style names say what they would need instead of silently
/// mapping onto the wrong one.
pub fn output_style(state: &mut ReplState, arg: &str) {
    let value = arg.trim().to_ascii_lowercase();
    match value.as_str() {
        "" | "status" => println!(
            "output style: {} (is_simple)",
            if state.is_simple {
                "concise"
            } else {
                "default"
            }
        ),
        "default" | "normal" => {
            state.is_simple = false;
            println!("output style: default (applies from the next turn)");
        }
        "concise" | "simple" => {
            state.is_simple = true;
            println!("output style: concise — is_simple (applies from the next turn)");
        }
        "explanatory" | "learning" | "explanatory-learning" => println!(
            "{}",
            format!(
                "{value} is not a style this build has — the one it does is `concise` (is_simple); \
                 for a teaching tone, ask for it in the prompt"
            )
            .dimmed()
        ),
        other => println!(
            "{} unknown style {other:?} — usage: /output-style [default|concise|status]",
            "✗".red()
        ),
    }
}

/// `/release-notes [n|version|latest]` — the changelog, one section at a
/// time (Claude Code's version picker, in the REPL's numbered style).
pub fn release_notes(state: &ReplState, arg: Option<&str>) {
    let sections = changelog_sections(CHANGELOG);
    if sections.is_empty() {
        println!("{}", "no release notes in this build".dimmed());
        return;
    }
    let Some(value) = arg.map(str::trim).filter(|v| !v.is_empty()) else {
        println!("{}", "release notes:".bold());
        for (index, (title, _)) in sections.iter().enumerate() {
            println!("  {}. {title}", index + 1);
        }
        println!(
            "{}",
            "  /release-notes <n|version|latest> prints one — the changelog file in this workspace is the full record"
                .dimmed()
        );
        return;
    };
    let picked = if value.eq_ignore_ascii_case("latest") {
        Some(0)
    } else if let Ok(index) = value.parse::<usize>() {
        index.checked_sub(1).filter(|i| *i < sections.len())
    } else {
        sections.iter().position(|(title, _)| {
            title
                .to_ascii_lowercase()
                .contains(&value.to_ascii_lowercase())
        })
    };
    match picked {
        Some(index) => {
            let (title, body) = &sections[index];
            println!("{}", title.bold());
            println!("{}", body.trim_end());
        }
        None => println!(
            "{} no release notes match {value:?} — /release-notes lists what this build carries",
            "✗".red()
        ),
    }
    let _ = state;
}

/// Split a Keep-a-Changelog file into `(title, body)` per `##` section.
fn changelog_sections(text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            sections.push((title.trim().to_string(), String::new()));
            continue;
        }
        if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections
}

/// `/reload-skills` — re-read the skill directories.
///
/// The available-skill list is part of the **system prompt**
/// (`prompts::build_system_prompt`'s `skills_block`), which the session
/// pins as its cache prefix — so this reports what is on disk now and says
/// plainly that the model sees it on the next prompt rebuild.
pub fn reload_skills(state: &mut ReplState) {
    let registry = codesmith_agent_runtime::skills::discover_for_workspace_and_dir(
        &state.workspace,
        &state.skills_dir,
    );
    let names: Vec<String> = registry
        .list()
        .iter()
        .map(|skill| skill.name.clone())
        .collect();
    let baseline = state.skill_baseline.get_or_insert_with(|| names.clone());
    let added: Vec<&String> = names
        .iter()
        .filter(|name| !baseline.contains(name))
        .collect();
    let removed: Vec<&String> = baseline
        .iter()
        .filter(|name| !names.contains(name))
        .collect();
    println!(
        "skills: {} available ({} added, {} removed since startup)",
        names.len(),
        added.len(),
        removed.len()
    );
    for name in added {
        println!("  + {name}");
    }
    for name in removed {
        println!("  - {name}");
    }
    *baseline = names;
    println!(
        "{}",
        "the skill list rides the system prompt: the model sees it after the next prompt rebuild, which starts the cache prefix cold"
            .dimmed()
    );
}

/// `/reload-plugins [--force]` — reload mods, dylib extensions, and skills.
///
/// `--force` answers the cache question in advance (Claude Code's flag has
/// the same job): without it the reload is still performed, and the note
/// about the prompt cache is printed because the tool catalog changed.
pub async fn reload_plugins(state: &ReplState, force: bool) {
    println!("{}", "reloading mods, extensions, and skills".bold());
    super::mods_cmds::mods_command(state, "reload").await;
    super::extension_cmds::extension_command(state, "reload").await;
    let registry = codesmith_agent_runtime::skills::discover_for_workspace_and_dir(
        &state.workspace,
        &state.skills_dir,
    );
    println!("skills: {} available", registry.list().len());
    if !force {
        println!(
            "{}",
            "the tool catalog changed, so the prompt cache starts cold on the next turn — /reload-plugins --force quietens this note"
                .dimmed()
        );
    }
}

/// `/plugins` — every extension surface in one listing: mods, dylib
/// extensions, and skills. Enabling and disabling stay with the command
/// that owns each store (`/mods`, `/extension`, `/skills`).
pub async fn plugins(state: &ReplState) {
    println!("{}", "mods (Rhai script packages)".bold());
    super::mods_cmds::mods_command(state, "list").await;
    println!();
    println!("{}", "extensions (dylib)".bold());
    super::extension_cmds::extension_command(state, "list").await;
    println!();
    println!("{}", "skills".bold());
    super::skills_cmds::list_skills(state, None).await;
}

/// `/terminal-setup` — what to change in the terminal itself.
///
/// Printed rather than applied: editing a terminal's own config is the
/// user's call, and the per-terminal instructions are what Claude Code's
/// `/terminal-setup` documents too (the same terminals send the same
/// bytes).
pub fn terminal_setup() {
    println!("{}", "terminal setup".bold());
    println!("  Shift+Enter for a newline (native in these terminals):");
    println!("    iTerm2 · WezTerm · Ghostty · Kitty · Warp · Apple Terminal · Windows Terminal");
    println!("    VS Code / Cursor / Zed / Alacritty: enable the terminal's Shift+Enter binding");
    println!("  Everywhere else:");
    println!("    Ctrl+J inserts a newline, and a line ending in `\\` continues the prompt");
    println!("  Option/Alt as Meta (needed for Alt+P model, Alt+T thinking, Alt+B/F word moves):");
    println!("    macOS: iTerm2 → Profiles → Keys → Left/Right Option key as Esc+");
    println!("           Terminal.app → Profiles → Keyboard → Use Option as Meta key");
    println!("    Windows/Linux: most terminals send Alt as Meta already");
    println!("  Apple Terminal: disable the audible bell (Settings → Profiles → Advanced)");
}

/// `/tasks` (alias `/bashes`) — everything running in the background in one
/// place: shell jobs, sub-agents, and durable tasks. Claude Code's `/tasks`
/// is this view; the three sources keep their own commands.
pub async fn tasks(state: &mut ReplState) {
    println!("{}", "background work".bold());
    jobs(state);
    super::cmd_agents(state);
    task(state, "list").await;
}

/// `/stop` — stop every piece of background work this session started.
///
/// Claude Code's `/stop` stops the background *session* you attached to;
/// background sessions are not implemented here (see `docs/CLI.md`), so the
/// name stops what this REPL actually runs in the background: shell jobs,
/// background sub-agents, and the durable tasks the task manager is running.
/// The foreground turn is untouched — `Ctrl+C` (or `Esc` mid-turn) is the key
/// for that.
pub async fn stop(state: &mut ReplState) {
    // Two stores hold background work: the engine's background-task registry
    // (shell jobs and background sub-agents) and the task manager (durable
    // tasks). `/tasks` prints both, so `/stop` has to reach both.
    let mut stopped_tasks = 0usize;
    let mut failed_tasks = 0usize;
    if let Some(manager) = state.task_manager.clone() {
        for task in manager.list_tasks(None).await {
            if matches!(
                task.status,
                crate::task_manager::TaskStatus::Completed
                    | crate::task_manager::TaskStatus::Failed
                    | crate::task_manager::TaskStatus::Canceled
            ) {
                continue;
            }
            match manager.cancel_task(&task.id).await {
                Ok(_) => stopped_tasks += 1,
                Err(_) => failed_tasks += 1,
            }
        }
    }

    let engine = match state.engine.as_ref() {
        Some(engine) => engine.clone(),
        None => {
            println!("{}", "✗ no engine in this session".red());
            return;
        }
    };
    match engine
        .send(codesmith_agent_runtime::ops::Op::CancelAllBackgroundTasks)
        .await
    {
        Ok(()) => {
            println!(
                "{}",
                format!(
                    "stopping background work — {stopped_tasks} durable task(s) cancelled{}; shell \
                     jobs and background sub-agents are stopping too (the engine reports the count \
                     as this turn's events render)",
                    if failed_tasks > 0 {
                        format!(" ({failed_tasks} could not be cancelled)")
                    } else {
                        String::new()
                    }
                )
                .dimmed()
            );
        }
        Err(err) => println!("{} could not reach the engine: {err}", "✗".red()),
    }
}

/// `/queue` — messages typed while a turn was running.
///
/// The queue is the mid-turn composer's: whatever the turn did not pick up
/// is sent as the next turn, and `Up` on an empty prompt takes it back for
/// editing instead.
pub fn queue(state: &ReplState) {
    if state.queued_messages.is_empty() {
        println!(
            "{}",
            "nothing queued — type while a turn runs and press Enter to queue a message".dimmed()
        );
        return;
    }
    println!(
        "{}",
        format!(
            "{} queued message(s) — Up takes them back:",
            state.queued_messages.len()
        )
        .bold()
    );
    for (index, text) in state.queued_messages.iter().enumerate() {
        let first = text.lines().next().unwrap_or(text);
        println!("  {}. {}", index + 1, first);
    }
}

/// `/stash` — the draft stash is a prompt key, not a command.
pub fn stash() {
    println!(
        "{}",
        "Ctrl+S stashes the draft; Ctrl+S again on an empty prompt restores it".dimmed()
    );
}

pub fn cycles() {
    println!(
        "{}",
        "cycle briefings tracking is not wired in the REPL yet — /cycles arrives with the background-task surface port".dimmed()
    );
}

pub fn relay() {
    println!(
        "{}",
        "relay (baton-pass to a teammate) is not wired in the REPL yet — use the team tools from a message".dimmed()
    );
}

pub fn restore() {
    println!(
        "{}",
        "the session restore picker is line-mode now — use /sessions then /resume <id-prefix>"
            .dimmed()
    );
}

pub fn change() {
    println!(
        "{}",
        "changelog: see CHANGELOG.md in the repository".dimmed()
    );
}

pub fn rlm() {
    println!(
        "{}",
        "RLM turns run through the model-facing rlm tools — ask the agent to use them in a message"
            .dimmed()
    );
}

/// The memory file this session reads: the KoD entrypoint (`MEMORY.md`
/// inside the memory dir) when Knowledge on Demand is enabled, the flat
/// memory file otherwise — mirrors the `remember` tool's branch.
pub(crate) fn memory_file(state: &ReplState) -> std::path::PathBuf {
    if state.kod_enabled {
        codesmith_agent_runtime::knowledge::paths::resolve_memory_entrypoint(&state.memory_dir)
    } else {
        state.memory_path.clone()
    }
}

/// `/memory [show|path|clear|edit|help]` — inspect, clear, or open the
/// memory file. `edit` deliberately just prints the editor command rather
/// than spawning an editor in-process.
pub fn memory(state: &ReplState, arg: Option<&str>) {
    let sub = arg.map(str::trim).unwrap_or("show").to_ascii_lowercase();
    let path = memory_file(state);
    match sub.as_str() {
        // Claude Code's "auto memory": whether the agent records memories at
        // all. The effective value comes from `Config::memory_enabled`, whose
        // cascade is env-first — so the report names the env variable that
        // is deciding when one is set, rather than showing a config toggle
        // that is being overridden.
        "auto" | "auto status" => {
            let enabled = state.config.memory_enabled();
            println!(
                "auto memory: {}",
                if enabled { "on".green() } else { "off".red() }
            );
            println!("  store:   {}", path.display());
            let decider = [
                "CODESMITH_DISABLE_AUTO_MEMORY",
                "CODESMITH_SIMPLE",
                "CODESMITH_REMOTE",
                "CODESMITH_MEMORY",
            ]
            .into_iter()
            .find(|name| std::env::var_os(name).is_some());
            match decider {
                Some(name) => {
                    println!("  decided by: {name} (the environment wins over [memory] enabled)")
                }
                None => {
                    println!("  decided by: [memory] enabled in config.toml, else the default (on)")
                }
            }
        }
        "auto on" | "auto off" => {
            let value = sub == "auto on";
            match super::config_cmds::persist_section_bool(
                state.config_path.as_deref(),
                "memory",
                "enabled",
                value,
            ) {
                Ok(written) => {
                    println!(
                        "auto memory = {} — saved to {}; it applies from the next session",
                        if value { "on" } else { "off" },
                        written.display()
                    );
                    println!(
                        "{}",
                        "an environment variable (CODESMITH_DISABLE_AUTO_MEMORY, CODESMITH_SIMPLE, CODESMITH_REMOTE) still wins over this"
                            .dimmed()
                    );
                }
                Err(err) => println!("{} could not write config.toml: {err}", "✗".red()),
            }
        }
        "" | "show" => {
            println!("memory file: {}", path.display());
            match std::fs::read_to_string(&path) {
                Ok(content) if content.trim().is_empty() => {
                    println!("{}", "(empty)".dimmed());
                }
                Ok(content) => println!("{content}"),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    println!("{}", "(empty — nothing remembered yet)".dimmed());
                }
                Err(err) => println!("{} read {}: {err}", "✗".red(), path.display()),
            }
        }
        "path" => println!("{}", path.display()),
        "clear" => {
            if let Some(parent) = path.parent()
                && let Err(err) = std::fs::create_dir_all(parent)
            {
                println!("{} clear failed: {err}", "✗".red());
                return;
            }
            match codesmith_agent_runtime::utils::write_atomic(&path, b"") {
                Ok(()) => println!("cleared {}", path.display()),
                Err(err) => println!("{} clear failed: {err}", "✗".red()),
            }
        }
        "edit" => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            println!("{editor} {}", path.display());
        }
        "help" => {
            println!(
                "{}",
                "usage: /memory [show|path|clear|edit] — the file is re-read at every prompt build, so edits land on the next turn".dimmed()
            );
            println!("memory file: {}", path.display());
        }
        other => println!(
            "{} unknown subcommand `{other}` — show | path | clear | edit | help",
            "✗".red()
        ),
    }
}

/// `#`-prefix quick memory: a line starting with exactly one `#` (but not
/// `##` or `#!`, so Markdown headings and shebangs fall through to a
/// normal turn) is a memory append — no turn fires.
pub(crate) fn quick_memory_entry(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('#')?;
    if rest.starts_with('#') || line.starts_with("#!") {
        return None;
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::{ReplState, attach, changelog_sections, quick_memory_entry};

    /// Round-3 TODO-41: `/attach` must reject media the wire layer cannot
    /// represent (video containers, tif, ppm, …) up front — the request
    /// builder silently drops such blocks, so accepting them told the user
    /// the model saw something it never did. Baseline: an .mp4 was queued.
    #[test]
    fn attach_rejects_wire_unrepresentable_media() {
        let _lock = codesmith_agent_runtime::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let mp4 = dir.path().join("clip.mp4");
        std::fs::write(&mp4, b"x").expect("write");
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"x").expect("write");

        fn options(workspace: std::path::PathBuf) -> crate::app_state::InteractiveOptions {
            crate::app_state::InteractiveOptions {
                model: "test-model".to_string(),
                workspace,
                config_path: None,
                config_profile: None,
                allow_shell: false,
                max_subagents: 2,
                skills_dir: std::env::temp_dir(),
                memory_path: std::env::temp_dir().join("memory.md"),
                mcp_config_path: std::env::temp_dir().join("mcp.json"),
                use_memory: false,
                start_in_agent_mode: true,
                yolo: false,
                resume_session_id: None,
                initial_input: None,
            }
        }
        let config = crate::config::Config::default();

        let mut state = ReplState::new(options(dir.path().to_path_buf()), &config);
        attach(&mut state, Some(mp4.to_str().expect("utf8")));
        assert!(
            state.pending_attachments.is_empty(),
            "mp4 must be rejected at /attach — the wire layer would silently drop it"
        );

        attach(&mut state, Some(png.to_str().expect("utf8")));
        assert_eq!(
            state.pending_attachments.len(),
            1,
            "png is wire-representable and must queue"
        );
    }

    #[test]
    fn quick_memory_entry_intercepts_single_hash_only() {
        assert_eq!(
            quick_memory_entry("# remember this"),
            Some(" remember this")
        );
        assert_eq!(quick_memory_entry("#no-space"), Some("no-space"));
        // Bare `#` is an (empty) entry — append_entry rejects it downstream.
        assert_eq!(quick_memory_entry("#"), Some(""));
    }

    #[test]
    fn quick_memory_entry_falls_through_headings_and_shebangs() {
        assert_eq!(quick_memory_entry("## heading"), None);
        assert_eq!(quick_memory_entry("### deep heading"), None);
        assert_eq!(quick_memory_entry("#!/bin/sh"), None);
        assert_eq!(quick_memory_entry("plain text"), None);
        assert_eq!(quick_memory_entry(""), None);
    }

    #[test]
    fn changelog_sections_split_on_level_two_headings() {
        let text = "# Changelog\n\nintro\n\n## [Unreleased]\n\n- one\n- two\n\n## [0.9.0] - 2026-01-01\n\n- three\n";
        let sections = changelog_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "[Unreleased]");
        assert!(sections[0].1.contains("- one"));
        assert!(sections[0].1.contains("- two"));
        assert!(
            !sections[0].1.contains("three"),
            "a section stops at the next heading"
        );
        assert_eq!(sections[1].0, "[0.9.0] - 2026-01-01");
        assert!(sections[1].1.contains("- three"));
        assert!(
            !sections[0].1.contains("intro"),
            "prose before the first heading belongs to no section"
        );
    }
}
