//! Slash-command dispatcher for the REPL.
//!
//! The full TUI command surface is being ported here command-by-command
//! against [`ReplState`]; the retired `commands/` tree coupled every
//! handler to the ratatui `App`. Handlers print directly to stdout and
//! mutate state / send engine ops.

pub mod auto_route;
pub mod btw_cmds;
pub mod config_cmds;
pub mod debug_cmds;
pub mod dir_cmds;
pub mod extension_cmds;
pub mod fewer_prompts_cmds;
pub mod import_cmds;
pub mod insights_cmds;
pub mod loop_cmds;
pub mod misc_cmds;
pub mod mods_cmds;
pub mod network_store;
pub mod permission_cmds;
pub mod pr_cmds;
pub mod project_doc;
pub mod registry;
pub mod rewind_cmds;
pub mod session_cmds;
pub mod share;
pub mod skills_cmds;
pub mod subtask_cmds;
pub mod usage_scan;
pub mod user_cmd_store;

use codesmith_config::presets::PresetCatalog;
use colored::Colorize;

use self::registry::Command;
use super::state::ReplState;
use crate::app_state::AppMode;
use crate::core::ops::Op;
use crate::presets;

/// Dispatch one `/command args…` line. Returns `true` when the REPL
/// should exit.
pub async fn dispatch(state: &mut ReplState, line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(raw) = parts.next() else {
        return false;
    };
    let command = raw.trim_start_matches('/');
    let arg_str = line[raw.len()..].trim().to_string();

    // User-defined commands override built-ins (mirrors the TUI's layer
    // order: user > extension > mods > static table).
    if let Some(handled) = skills_cmds::try_user_command(state, command, &arg_str).await
        && handled
    {
        return false;
    }

    // Extension/mod commands sit between user commands and the built-in
    // table (the layer order above): dispatched against the live runner,
    // so a mod command activated this session works immediately. A name
    // lookup first keeps a run() failure from falling through to the
    // misleading "unknown command" arm.
    if let Some(runner) = state.extension_runner.clone()
        && runner.bound_command_names().iter().any(|n| n == command)
    {
        use codesmith_agent::extension::CommandOutput;
        match runner.try_dispatch_command(command, &arg_str).await {
            Some(CommandOutput::Message(text)) => {
                println!("{text}");
                return false;
            }
            Some(CommandOutput::SendMessage(text)) => {
                let _ = super::submit(state, text).await;
                return false;
            }
            None => {
                println!("{} command /{command} failed — see the log", "✗".red());
                return false;
            }
        }
    }

    // Built-ins resolve through the registry: every name `/help` prints and
    // Tab completes is dispatchable, and a registered name with no arm below
    // is a compile error because this match is exhaustive over `Command`.
    let Some(info) = Command::resolve(command) else {
        println!(
            "{} unknown command /{command} — {}",
            "✗".red(),
            "/help lists what exists".dimmed()
        );
        return false;
    };
    // The builtin layer's disable seam: config (`[commands] disabled`) or
    // a live mod registration (`disable_command`). Reached only after the
    // user/mod layers above, so disabling targets the builtin row alone.
    if builtin_disabled(state, info) {
        println!(
            "{} /{} is disabled — [commands] disabled in config.toml or a mod's disable_command",
            "✗".red(),
            info.name
        );
        return false;
    }
    match info.command {
        Command::Help => {
            print_help(state);
            false
        }
        Command::Exit => true,
        Command::Clear => {
            print!("\x1b[2J\x1b[H");
            // The screen transcript is gone; so is its replay source.
            state.tool_log.clear();
            false
        }
        Command::Status => {
            print_status(state);
            false
        }
        Command::Expand => {
            cmd_expand(state, &arg_str);
            false
        }
        Command::Model => {
            cmd_model(state, &arg_str).await;
            false
        }
        Command::Mode => {
            cmd_mode(state, &arg_str);
            false
        }
        // Upstream #1247 kept /plan and /agent as compatibility shorthands
        // for /mode; the line-REPL port had dropped the first two.
        Command::Plan | Command::Agent => {
            cmd_mode(state, info.name);
            false
        }
        Command::Yolo => {
            state.set_mode(AppMode::Yolo);
            println!("{}", "YOLO mode: shell + trust + auto-approve ON".yellow());
            false
        }
        Command::Preset => {
            cmd_preset(state, &arg_str);
            false
        }
        Command::Provider => {
            cmd_provider(state, &arg_str);
            false
        }
        Command::Sessions => {
            session_cmds::sessions(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Resume => {
            session_cmds::resume(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Save => {
            session_cmds::save(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Fork => {
            session_cmds::fork(state).await;
            false
        }
        Command::Branch => {
            session_cmds::branch(state, Some(arg_str.as_str())).await;
            false
        }
        Command::Subtask => {
            subtask_cmds::run(state, &arg_str).await;
            false
        }
        Command::Batch => {
            skills_cmds::batch(state, &arg_str).await;
            false
        }
        Command::FewerPermissionPrompts => {
            fewer_prompts_cmds::run(state, &arg_str).await;
            false
        }
        Command::Stop => {
            misc_cmds::stop(state).await;
            false
        }
        Command::SkillDoctor => {
            skills_cmds::skill_doctor(state);
            false
        }
        Command::RunSkillGenerator => {
            skills_cmds::bundled(state, "run-skill-generator", &arg_str).await;
            false
        }
        Command::UpdateConfig => {
            skills_cmds::bundled(state, "update-config", &arg_str).await;
            false
        }
        Command::DeepResearch => {
            if arg_str.trim().is_empty() {
                println!(
                    "{}",
                    "usage: /deep-research <question> — searches, cross-checks the sources, and \
                     writes a cited answer"
                        .dimmed()
                );
            } else {
                skills_cmds::bundled(state, "deep-research", &arg_str).await;
            }
            false
        }
        Command::PluginAuthoring => {
            skills_cmds::bundled(state, "plugin-authoring", &arg_str).await;
            false
        }
        Command::Load => {
            session_cmds::load(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Compact => {
            session_cmds::compact(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Purge => {
            session_cmds::purge(state).await;
            false
        }
        Command::New => {
            session_cmds::new_session(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Export => {
            session_cmds::export(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Rename => {
            session_cmds::rename(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Agents => {
            cmd_agents(state);
            false
        }
        Command::Logout => {
            cmd_logout(state);
            false
        }
        Command::Tokens => {
            debug_cmds::tokens(state);
            false
        }
        Command::Cost => {
            debug_cmds::usage(state);
            false
        }
        Command::Tools => {
            debug_cmds::tools(state);
            false
        }
        Command::System => {
            debug_cmds::system_prompt(state);
            false
        }
        Command::Context => {
            debug_cmds::context(state, Some(arg_str.as_str()));
            false
        }
        Command::Cache => {
            debug_cmds::cache(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Diff => {
            debug_cmds::diff(state);
            false
        }
        Command::Edit => {
            debug_cmds::edit(state);
            false
        }
        Command::Undo => {
            debug_cmds::undo(state).await;
            false
        }
        Command::Retry => {
            debug_cmds::retry(state).await;
            false
        }
        Command::Config => {
            config_cmds::config_command(state, &arg_str);
            false
        }
        Command::Settings => {
            config_cmds::show_settings(state);
            false
        }
        Command::Verbose => {
            config_cmds::verbose(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Trust => {
            config_cmds::trust(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Slop => {
            config_cmds::slop(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Lsp => {
            config_cmds::lsp(state);
            false
        }
        Command::Theme => {
            crate::interactive::theme_command(state, &arg_str, has_save_flag(&arg_str));
            false
        }
        Command::Statusline => {
            crate::interactive::statusline_command(state, &arg_str);
            false
        }
        Command::Color => {
            crate::interactive::color_command(state, &arg_str, has_save_flag(&arg_str));
            false
        }
        Command::Doctor => {
            crate::interactive::doctor_command(state, &arg_str).await;
            false
        }
        Command::Debug => {
            crate::interactive::debug_command(state, &arg_str);
            false
        }
        Command::Copy => {
            crate::interactive::copy_command(state, &arg_str);
            false
        }
        Command::Import => {
            let yes = arg_str
                .split_whitespace()
                .any(|word| word == "--yes" || word == "-y");
            import_cmds::run(state, &arg_str, yes);
            false
        }
        Command::Cd => {
            if let Err(err) = dir_cmds::cd(state, &arg_str).await {
                println!("{} /cd failed: {err}", "✗".red());
            }
            false
        }
        Command::AddDir => {
            if let Err(err) = dir_cmds::add_dir(state, &arg_str) {
                println!("{} /add-dir failed: {err}", "✗".red());
            }
            false
        }
        Command::Permissions => {
            let mut parts = arg_str.split_whitespace();
            match parts.next().unwrap_or("list").to_ascii_lowercase().as_str() {
                "list" | "" | "show" => permission_cmds::list(state),
                "allow" => {
                    permission_cmds::allow(state, parts.collect::<Vec<_>>().join(" ").as_str())
                }
                "deny" => {
                    permission_cmds::deny(state, parts.collect::<Vec<_>>().join(" ").as_str())
                }
                "remove" | "revoke" => {
                    permission_cmds::remove(state, parts.collect::<Vec<_>>().join(" ").as_str())
                }
                other => println!(
                    "{} unknown subcommand {other:?} — usage: /permissions [list|allow <prefix>|deny <command>|remove <prefix>]",
                    "✗".red()
                ),
            }
            false
        }
        Command::Recap => {
            misc_cmds::recap(state).await;
            false
        }
        Command::Insights => {
            insights_cmds::run(&arg_str).await;
            false
        }
        Command::Pr => {
            pr_cmds::run(state, &arg_str).await;
            false
        }
        Command::Loop => {
            loop_cmds::run(state, &arg_str).await;
            false
        }
        Command::OutputStyle => {
            misc_cmds::output_style(state, &arg_str);
            false
        }
        Command::Btw => {
            if let Err(err) = btw_cmds::ask(state, &arg_str).await {
                println!("{} /btw failed: {err}", "✗".red());
            }
            false
        }
        Command::Rewind => {
            rewind(state, &arg_str).await;
            false
        }
        Command::Keybindings => {
            config_cmds::keybindings();
            false
        }
        Command::Effort => {
            config_cmds::effort(state, Some(arg_str.as_str()));
            false
        }
        Command::Autocompact => {
            config_cmds::autocompact(state, Some(arg_str.as_str())).await;
            false
        }
        Command::Sandbox => {
            let (arg, persist) = split_save_flag(&arg_str);
            config_cmds::sandbox(state, arg, persist);
            false
        }
        Command::ReleaseNotes => {
            misc_cmds::release_notes(state, Some(arg_str.as_str()));
            false
        }
        Command::TerminalSetup => {
            misc_cmds::terminal_setup();
            false
        }
        Command::Tasks => {
            misc_cmds::tasks(state).await;
            false
        }
        Command::ReloadSkills => {
            misc_cmds::reload_skills(state);
            false
        }
        Command::ReloadPlugins => {
            let force = arg_str
                .split_whitespace()
                .any(|part| part == "--force" || part == "-f");
            misc_cmds::reload_plugins(state, force).await;
            false
        }
        Command::Plugins => {
            misc_cmds::plugins(state).await;
            false
        }
        Command::Init => {
            misc_cmds::init(state);
            false
        }
        Command::Hooks => {
            misc_cmds::hooks(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Mcp => {
            misc_cmds::mcp(state, &arg_str).await;
            false
        }
        Command::Network => {
            misc_cmds::network(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Goal => {
            misc_cmds::goal(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Task => {
            misc_cmds::task(state, &arg_str).await;
            false
        }
        Command::Jobs => {
            misc_cmds::jobs(state);
            false
        }
        Command::Balance => {
            misc_cmds::balance(state);
            false
        }
        Command::Verify | Command::RunApp => {
            let target = arg_str.as_str();
            if matches!(info.command, Command::Verify) {
                skills_cmds::activate(state, "verify-app", target).await;
            } else {
                skills_cmds::run_app(state, target).await;
            }
            false
        }
        Command::ClaimCheck => {
            misc_cmds::verify(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::CodeReview => {
            skills_cmds::code_review(state, &arg_str).await;
            false
        }
        Command::SecurityReview => {
            skills_cmds::security_review(state, &arg_str).await;
            false
        }
        Command::Simplify => {
            skills_cmds::simplify(state, &arg_str).await;
            false
        }
        Command::Links => {
            misc_cmds::links();
            false
        }
        Command::Home => {
            misc_cmds::home(state);
            false
        }
        Command::Profile => {
            misc_cmds::profile(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Workspace => {
            misc_cmds::workspace(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Translate => {
            misc_cmds::translate(state);
            false
        }
        Command::Anchor => {
            misc_cmds::anchor(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Note => {
            misc_cmds::note(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Attach => {
            misc_cmds::attach(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Feedback => {
            misc_cmds::feedback(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Share => {
            misc_cmds::share(state).await;
            false
        }
        Command::Skills => {
            skills_cmds::list_skills(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Skill => {
            skills_cmds::run_skill(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Review => {
            skills_cmds::review(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Queue => {
            misc_cmds::queue(state);
            false
        }
        Command::Stash => {
            misc_cmds::stash();
            false
        }
        Command::Cycles => {
            misc_cmds::cycles();
            false
        }
        Command::Relay => {
            misc_cmds::relay();
            false
        }
        Command::Restore => {
            misc_cmds::restore();
            false
        }
        Command::Change => {
            misc_cmds::change();
            false
        }
        Command::Rlm => {
            misc_cmds::rlm();
            false
        }
        Command::Extension => {
            extension_cmds::extension_command(state, &arg_str).await;
            false
        }
        Command::Mods => {
            mods_cmds::mods_command(state, &arg_str).await;
            false
        }
        Command::Models => {
            let json = arg_str.split_whitespace().any(|flag| flag == "--json");
            if let Err(err) =
                crate::main_root::run_models(&state.config, crate::main_root::ModelsArgs { json })
                    .await
            {
                println!("{} {err}", "✗".red());
            }
            false
        }
        Command::Memory => {
            misc_cmds::memory(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Set => {
            println!(
                "{}",
                "the /set command was retired — use /config to edit settings and /settings to inspect values".dimmed()
            );
            false
        }
        Command::Deepseek => {
            println!(
                "{}",
                "the /deepseek command was renamed — use /links (aliases: /dashboard, /api)"
                    .dimmed()
            );
            false
        }
    }
}

/// Resolve the `[commands] disabled` config entries to canonical builtin
/// names, or fail loud naming the entries that match no builtin (a typo'd
/// disable would otherwise silently disable nothing). Aliases resolve to
/// their canonical name; duplicates collapse.
pub fn canonicalize_disabled(entries: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        match Command::resolve(entry) {
            Some(info) => {
                let name = info.name.to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            None => {
                return Err(format!(
                    "[commands] disabled: {entry:?} matches no built-in command \
                     (canonical names and aliases; /help lists them)"
                ));
            }
        }
    }
    Ok(out)
}

/// Whether the builtin row is disabled right now — by config
/// (`[commands] disabled`, canonicalized at REPL start) or by a live mod
/// (`disable_command`; cleared and re-derived on reload/unload). The user
/// and mod layers dispatch before this check, so a same-named user or mod
/// command still works: disabling targets the builtin row alone.
fn builtin_disabled(state: &ReplState, info: &registry::CommandInfo) -> bool {
    state.disabled_commands.iter().any(|name| name == info.name)
        || state
            .extension_runner
            .as_ref()
            .is_some_and(|runner| runner.is_builtin_command_disabled(info.name))
}

/// Whether an argument string asks to persist (`--save` / `-s`).
fn has_save_flag(arg: &str) -> bool {
    arg.split_whitespace()
        .any(|part| part == "--save" || part == "-s")
}

/// Split a trailing `--save` / `-s` off an argument string.
fn split_save_flag(arg: &str) -> (Option<&str>, bool) {
    let trimmed = arg.trim();
    for flag in ["--save", "-s"] {
        if let Some(rest) = trimmed.strip_suffix(flag) {
            return (
                Some(rest.trim_end()).filter(|value| !value.is_empty()),
                true,
            );
        }
    }
    (Some(trimmed).filter(|value| !value.is_empty()), false)
}

/// `/rewind [n|list]` — list or restore this session's file snapshots.
///
/// Files only, by design: the conversation half is `/undo` and `/retry` (see
/// `rewind_cmds` for why the two are not fused).
async fn rewind(state: &mut ReplState, arg: &str) {
    let rows = match rewind_cmds::list(&state.workspace).await {
        Ok(rows) => rows,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return;
        }
    };
    if rows.is_empty() {
        println!(
            "{}",
            "no snapshots to rewind to — they are written around file-mutating turns (see [snapshots] in config.toml)"
                .dimmed()
        );
        return;
    }
    let value = arg.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("list") {
        println!("{}", "file snapshots (newest first):".bold());
        for (index, row) in rows.iter().enumerate() {
            println!(
                "  {}. {}  {}  {}",
                index + 1,
                row.short_id().dimmed(),
                row.when(),
                row.label
            );
        }
        println!(
            "{}",
            "  /rewind <n> restores the workspace to that snapshot — files only; /undo drops the last exchange"
                .dimmed()
        );
        return;
    }
    let picked = if let Ok(index) = value.parse::<usize>() {
        index.checked_sub(1).filter(|i| *i < rows.len())
    } else {
        rows.iter()
            .position(|row| row.id.starts_with(value) || row.short_id() == value)
    };
    let Some(index) = picked else {
        println!(
            "{} no snapshot matches {value:?} — /rewind lists them",
            "✗".red()
        );
        return;
    };
    let row = &rows[index];
    println!(
        "restoring the workspace to {} ({})",
        row.short_id(),
        row.label
    );
    match rewind_cmds::restore(&state.workspace, row.id.clone()).await {
        Ok(()) => println!(
            "{}",
            "workspace restored — the conversation was not rewound; /undo drops the last exchange"
                .green()
        ),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// `/help` — every registered command, straight from
/// [`registry::Command::ALL`], grouped by [`registry::Group`].
///
/// Table-driven so it cannot fall behind the dispatcher (it was a
/// hardcoded 20-row subset of the 75 dispatchable names) and so it names
/// exactly what Tab completes.
fn print_help(state: &ReplState) {
    println!(
        "{}",
        "commands (Tab completes these names; Shift+Tab cycles the run mode):".bold()
    );
    let disabled_count = registry::Command::ALL
        .iter()
        .filter(|info| builtin_disabled(state, info))
        .count();
    for group in registry::Group::ALL {
        let rows: Vec<&registry::CommandInfo> = registry::Command::ALL
            .iter()
            .filter(|info| info.group == *group && !builtin_disabled(state, info))
            .collect();
        if rows.is_empty() {
            continue;
        }
        println!();
        println!("{}", format!("{}:", group.label()).bold());
        for info in rows {
            let name = if info.usage.is_empty() {
                format!("/{}", info.name)
            } else {
                format!("/{} {}", info.name, info.usage)
            };
            if name.len() <= 32 {
                println!("  {name:<32} {}", info.help.dimmed());
            } else {
                println!("  {name}");
                println!("    {}", info.help.dimmed());
            }
        }
    }
    if disabled_count > 0 {
        println!();
        println!(
            "{}",
            format!("{disabled_count} command(s) disabled ([commands] disabled in config.toml or a mod's disable_command)")
                .dimmed()
        );
    }
    let user = user_cmd_store::user_command_names(Some(&state.workspace));
    if !user.is_empty() {
        println!();
        println!("{}", "user commands (these shadow built-ins):".bold());
        for name in user {
            println!("  /{name}");
        }
    }
    // Mod commands dispatch through the layer between user commands and
    // the static table; listing them here keeps `/help` naming everything
    // Tab completes.
    if let Some(runner) = &state.extension_runner {
        let mut mods = runner.bound_command_names();
        if !mods.is_empty() {
            mods.sort();
            println!();
            println!("{}", "mod commands (from script mods):".bold());
            for name in mods {
                println!("  /{name}");
            }
        }
    }
}

/// `/expand [n|all]` — reprint the detail hidden by collapsed tool-call
/// lines. Bare: every call from the most recent turn that logged one;
/// `n`: that turn's n-th call; `all`: the whole retained log.
fn cmd_expand(state: &ReplState, arg: &str) {
    let arg = arg.trim();
    if state.tool_log.is_empty() {
        println!("{}", "no tool calls recorded this session".dimmed());
        return;
    }
    let last_turn = state
        .tool_log
        .iter()
        .map(|record| record.turn)
        .max()
        .expect("tool_log is non-empty");
    let records: Vec<crate::tool_summary::ToolCallRecord> = if arg.eq_ignore_ascii_case("all") {
        state.tool_log.clone()
    } else if let Some(index) = arg.parse::<usize>().ok().filter(|n| *n > 0) {
        let turn_records: Vec<_> = state
            .tool_log
            .iter()
            .filter(|record| record.turn == last_turn)
            .collect();
        match turn_records.get(index - 1) {
            Some(record) => vec![(**record).clone()],
            None => {
                println!(
                    "{} no tool call #{index} in the last turn (it logged {})",
                    "✗".red(),
                    turn_records.len()
                );
                return;
            }
        }
    } else if arg.is_empty() {
        state
            .tool_log
            .iter()
            .filter(|record| record.turn == last_turn)
            .cloned()
            .collect()
    } else {
        println!("{}", "usage: /expand [n|all]".dimmed());
        return;
    };
    if records.is_empty() {
        println!("{}", "no tool calls recorded this session".dimmed());
        return;
    }
    println!("{}", "tool-call detail:".bold());
    for record in &records {
        println!(
            "{}",
            crate::tool_summary::tool_detail_block(record, crate::tool_summary::DETAIL_MAX_LINES)
                .dimmed()
        );
    }
}

fn print_status(state: &ReplState) {
    println!("{}", "session status".bold());
    println!("  model:            {}", state.model_display_label());
    println!("  provider:         {}", state.api_provider.as_str());
    println!("  mode:             {}", state.mode.label());
    println!(
        "  preset:           {}",
        state.active_preset.as_deref().unwrap_or("(none)")
    );
    println!(
        "  reasoning:        {}",
        state.reasoning_effort_display_label()
    );
    println!("  running agents:   {}", state.running_agent_count());
    match state.current_session_id.as_deref() {
        Some(id) => println!(
            "  session:          {}",
            crate::session_manager::truncate_id(id)
        ),
        None => println!("  session:          (unsaved)"),
    }
    println!(
        "  tokens in/out:    {}/{}",
        state.session.total_input_tokens, state.session.total_output_tokens
    );
    println!("  cost (usd):       {:.4}", state.session.session_cost);
    println!("  worked:           {:?}", state.cumulative_turn_duration);
    println!(
        "  memory/kod/lsp:   {}/{}/{}",
        state.use_memory, state.kod_enabled, state.lsp_enabled
    );
    if state.use_memory {
        println!("  memory file:      {}", state.memory_path.display());
    }
    if state.kod_enabled {
        println!("  memory dir:       {}", state.memory_dir.display());
    }
    println!("  mcp servers:      {}", state.mcp_configured_count);
    if state.mcp_configured_count > 0 {
        println!("  mcp config:       {}", state.mcp_config_path.display());
    }
    if let Some(path) = state.config_path.as_ref() {
        println!(
            "  config file:      {} ({})",
            path.display(),
            state.config_profile.as_deref().unwrap_or("default profile")
        );
    }
    if state.yolo {
        println!("  yolo:             {}", "ON".yellow());
    }
}

async fn cmd_model(state: &mut ReplState, arg: &str) {
    if arg.trim().is_empty() {
        println!("model: {}", state.model_display_label());
        if state.auto_model {
            println!(
                "{}",
                "auto routing is ON; the effective model is picked per request".dimmed()
            );
        }
        return;
    }
    let selection = arg.trim().to_string();
    if !state.accepts_custom_model_ids() && selection != "auto" {
        println!(
            "{}",
            format!(
                "note: provider {} may reject unknown model ids",
                state.api_provider.as_str()
            )
            .dimmed()
        );
    }
    state.set_model_selection(selection.clone());
    if let Ok(engine) = state.engine() {
        let _ = engine.send(Op::SetModel { model: selection }).await;
    }
    // Persist the selection (mirrors the retired /model command): pinned
    // per provider in settings.toml so restarts keep it.
    match crate::settings::Settings::load() {
        Ok(mut settings) => {
            let provider = state.api_provider.as_str().to_string();
            settings
                .provider_models
                .get_or_insert_with(Default::default)
                .insert(provider, state.model_selection_for_persistence());
            if let Err(err) = settings.save() {
                println!(
                    "{}",
                    format!("warning: could not persist model selection: {err}").yellow()
                );
            }
        }
        Err(err) => println!(
            "{}",
            format!("warning: could not load settings to persist model: {err}").yellow()
        ),
    }
    println!("model set to {}", state.model_display_label());
}

fn cmd_mode(state: &mut ReplState, arg: &str) {
    let Some(name) = arg.split_whitespace().next() else {
        println!("mode: {} (Shift+Tab cycles)", state.mode.label());
        return;
    };
    let mode = match name.to_ascii_lowercase().as_str() {
        "agent" => AppMode::Agent,
        "yolo" => AppMode::Yolo,
        "plan" => AppMode::Plan,
        "coordinator" => AppMode::Coordinator,
        other => {
            println!(
                "{} unknown mode {other:?} (agent | yolo | plan | coordinator)",
                "✗".red()
            );
            return;
        }
    };
    state.set_mode(mode);
    println!("switched to {} mode", state.mode.label());
}

fn cmd_preset(state: &mut ReplState, arg: &str) {
    let arg = arg.trim();
    if let Some(name) = arg.strip_prefix("export").map(str::trim) {
        export_preset(state, name);
        return;
    }
    if arg == "off" {
        preset_off(state);
        return;
    }
    if arg.is_empty() || arg == "list" {
        let catalog = PresetCatalog::load(Some(&state.workspace));
        print!(
            "{}",
            presets::describe(&catalog, state.active_preset.as_deref())
        );
        return;
    }
    let name = arg.to_string();
    state.apply_preset_at_startup(&name);
    match state.active_preset.as_deref() {
        Some(applied) if applied == name => {
            println!("preset: {applied} (live dials applied)");
            // Persist the selection so the next start picks it up (mirrors
            // the retired /preset command, which wrote
            // `settings.active_preset`).
            match crate::settings::Settings::load() {
                Ok(mut settings) => {
                    settings.active_preset = Some(name);
                    if let Err(err) = settings.save() {
                        println!(
                            "{}",
                            format!("warning: could not persist preset selection: {err}").yellow()
                        );
                    }
                }
                Err(err) => println!(
                    "{}",
                    format!("warning: could not load settings to persist preset: {err}").yellow()
                ),
            }
        }
        Some(applied) => {
            println!(
                "{}",
                format!("preset {name:?} not found; active preset is {applied:?} (diy deviations report as diy)").yellow()
            );
        }
        None => println!(
            "{} preset {name:?} not found; no preset is active",
            "✗".red()
        ),
    }
}

/// `/preset export <name>` — snapshot the current dials as a user-layer
/// preset file (`~/.codesmith/presets/<name>.toml`): the active definition
/// as the base, overlaid with the live dials the REPL tracks (mode + tool
/// lists).
fn export_preset(state: &mut ReplState, name: &str) {
    if name.is_empty() {
        println!(
            "{}",
            "usage: /preset export <name> — snapshots the current dials as a shareable preset"
                .dimmed()
        );
        return;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        println!("{} invalid preset name {name:?}", "✗".red());
        return;
    }
    let catalog = PresetCatalog::load(Some(&state.workspace));
    let base = state
        .active_preset
        .as_deref()
        .and_then(|active| catalog.get_ci(active))
        .map(|loaded| loaded.definition.clone());
    let definition = presets::export_definition(
        base.as_ref(),
        state.mode.as_setting(),
        state.active_allowed_tools.as_deref(),
        state.active_blocked_tools.as_deref(),
        name,
    );
    let Ok(home) = codesmith_config::codesmith_home() else {
        println!("{} cannot resolve the CodeSmith home directory", "✗".red());
        return;
    };
    let dir = home.join("presets");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        println!("{} create {}: {err}", "✗".red(), dir.display());
        return;
    }
    let body = match toml::to_string_pretty(&definition) {
        Ok(body) => body,
        Err(err) => {
            println!("{} serialize preset: {err}", "✗".red());
            return;
        }
    };
    let path = dir.join(format!("{name}.toml"));
    if let Err(err) = std::fs::write(&path, body) {
        println!("{} write {}: {err}", "✗".red(), path.display());
        return;
    }
    println!(
        "exported preset {name} → {} (user layer; /preset {name} applies it)",
        path.display()
    );
}

/// `/preset off` — drop the preset layer, keep the current dials: clears
/// the session's preset state and the persisted selection. The next start
/// uses the default tier (`middle`) unless config.toml / `--preset` picks
/// one.
fn preset_off(state: &mut ReplState) {
    state.active_preset = None;
    state.active_allowed_tools = None;
    state.active_blocked_tools = None;
    match crate::settings::Settings::load() {
        Ok(mut settings) => {
            settings.active_preset = None;
            if let Err(err) = settings.save() {
                println!(
                    "{}",
                    format!("warning: could not persist preset selection: {err}").yellow()
                );
            }
        }
        Err(err) => println!(
            "{}",
            format!("warning: could not load settings to clear preset: {err}").yellow()
        ),
    }
    println!(
        "{}",
        "preset layer off — current dials kept; next start uses the default tier (middle) unless config.toml picks one".dimmed()
    );
}

fn cmd_provider(state: &mut ReplState, arg: &str) {
    if arg.trim().is_empty() {
        println!("provider: {}", state.api_provider.as_str());
        println!(
            "{}",
            "switching providers takes effect for new sessions; restart after /provider <name> for a full re-config".dimmed()
        );
        return;
    }
    match crate::config::ApiProvider::parse(arg.trim()) {
        Some(provider) => {
            state.api_provider = provider;
            println!("provider set to {}", state.api_provider.as_str());
            println!(
                "{}",
                "restart codesmith (or run the onboarding again) to rebuild the client for the new provider".dimmed()
            );
        }
        None => println!("{} unknown provider {arg:?}", "✗".red()),
    }
}

/// `/agents` — running sub-agents, plus the two configuration surfaces that
/// decide how they run.
///
/// Sub-agents in CodeSmith are spawned per task with a prompt (`agent_open`);
/// there is no `.claude/agents/*.md`-style definition file, so what can be
/// configured is the model each role uses and where its memory lives —
/// Claude Code's `/agents` covers definitions the model writes at spawn time.
fn cmd_agents(state: &ReplState) {
    if state.agent_progress.is_empty() {
        println!("no sub-agents running");
    } else {
        println!("{}", "running sub-agents:".bold());
        for (id, status) in &state.agent_progress {
            println!("  {id}: {status}");
        }
    }
    let overrides = state.config.subagent_model_overrides();
    if overrides.is_empty() {
        println!(
            "{}",
            "no per-role model overrides ([subagents] default_model / worker_model in config.toml)"
                .dimmed()
        );
    } else {
        println!("{}", "model overrides:".bold());
        let mut roles: Vec<(&String, &String)> = overrides.iter().collect();
        roles.sort();
        for (role, model) in roles {
            println!("  {role} → {model}");
        }
    }
    // The two roots `agent_memory_candidates` prefers, in its own order.
    let roots = [
        state.workspace.join(".codesmith/agent-memory"),
        state.workspace.join(".claude/agent-memory"),
    ];
    for root in roots {
        if root.is_dir() {
            println!("{}", format!("agent memory: {}", root.display()).dimmed());
        }
    }
}

fn cmd_logout(_state: &mut ReplState) {
    // Wipe the stored key and the onboarding marker so the next start is a
    // clean first run: the next bare `codesmith` re-asks the setup
    // questions. Signing back in is explicit — `codesmith auth set` or
    // `codesmith login` skips the flow.
    match crate::config::clear_api_key() {
        Ok(()) => {
            if let Some(marker) = codesmith_agent_runtime::onboarding::default_marker_path()
                && let Err(err) = std::fs::remove_file(&marker)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                println!(
                    "{}",
                    format!("warning: could not remove onboarding marker: {err}").yellow()
                );
            }
            println!(
                "{}",
                "logged out: API key removed; sign back in with `codesmith auth set` or `codesmith login`".green()
            );
        }
        Err(err) => println!("{} logout failed: {err}", "✗".red()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[commands] disabled` entries canonicalize (aliases resolve to the
    /// canonical name, duplicates collapse) or fail loud naming the
    /// unknown entry — the config side of the builtin-disable seam.
    #[test]
    fn disabled_entries_canonicalize_or_fail_loud() {
        let entries: Vec<String> = ["cost".into(), "stats".into(), "model".into()].to_vec();
        assert_eq!(
            canonicalize_disabled(&entries).unwrap(),
            ["usage", "model"],
            "cost/stats are aliases of usage; the duplicate collapses"
        );
        let err = canonicalize_disabled(&["nope".to_string()]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }
}
