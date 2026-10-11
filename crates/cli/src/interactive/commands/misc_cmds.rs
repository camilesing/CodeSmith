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
                "hook events: session_start, session_end, message_submit, tool_call_before, tool_call_after, mode_change, on_error, shell_env, task_created, task_completed, pre_compact".dimmed()
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
    state.mcp_configured_count = crate::mcp::load_config(&state.mcp_config_path)
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

/// `/workspace [path]` — switching needs an engine rebuild.
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
            "workspace switches need a restart: cd {} && codesmith (or codesmith --workspace {})",
            candidate.display(),
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
        match std::fs::write(&path, anchors.join("\n---\n")) {
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
    match std::fs::write(&path, content) {
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
            let _ = std::fs::write(&path, notes.join("\n---\n"));
            println!("removed note #{index}");
        }
        "clear" => {
            let _ = std::fs::write(&path, "");
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
            match std::fs::write(&path, content) {
                Ok(()) => println!("note added"),
                Err(err) => println!("{} failed to write notes: {err}", "✗".red()),
            }
        }
    }
}

/// `/attach <media path>` — queue an image/video for the next message.
pub fn attach(state: &mut ReplState, arg: Option<&str>) {
    let Some(raw_path) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        println!("{} usage: /attach <image-or-video-path>", "✗".red());
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
    if !path.is_file() {
        println!("{} attachment is not a file: {}", "✗".red(), path.display());
        return;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let is_media = matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "tif"
            | "tiff"
            | "ppm"
            | "mp4"
            | "mov"
            | "m4v"
            | "webm"
            | "avi"
            | "mkv"
    );
    if !is_media {
        println!(
            "{} unsupported attachment type — /attach is for image/video paths; use @path in the message for text files",
            "✗".red()
        );
        return;
    }
    state.pending_attachments.push(path.clone());
    println!("attached: {} (sent with your next message)", path.display());
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
            crate::utils::display_path(path)
        );
    }
    format!(
        "{}\nlog: {}",
        codesmith_agent_runtime::evolution_log::summarize(&records).render(),
        crate::utils::display_path(path)
    )
}

/// `/feedback [bug|feature|security|…]` — prints the issue URLs.
pub fn feedback(arg: Option<&str>) {
    let raw = arg.map(str::trim).unwrap_or("");
    println!(
        "feedback:\n  bug:      https://github.com/camilesing/CodeSmith/issues/new?template=bug_report.md\n  feature:  https://github.com/camilesing/CodeSmith/issues/new?template=feature_request.md\n  security: see SECURITY.md before reporting vulnerabilities"
    );
    if !raw.is_empty() && raw != "help" {
        println!(
            "{}",
            format!("(category '{raw}' noted; open the matching URL above)").dimmed()
        );
    }
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

pub fn queue() {
    println!(
        "{}",
        "the offline queue is a composer feature with no line-mode equivalent yet — messages sent during a turn steer/cancel via Ctrl+C".dimmed()
    );
}

pub fn stash() {
    println!(
        "{}",
        "draft stash is a composer feature with no line-mode equivalent yet".dimmed()
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

pub fn extension() {
    println!(
        "{}",
        "extension management (/extension list|reload|status) is not wired in the REPL yet — extension commands arrive with the §F port".dimmed()
    );
}

pub fn mods() {
    println!(
        "{}",
        "mods management (/mods list|status|activate) is not wired in the REPL yet — mod commands arrive with the §F port".dimmed()
    );
}
