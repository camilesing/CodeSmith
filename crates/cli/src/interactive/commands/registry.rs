//! Slash-command registry — the single source of truth for REPL command
//! names, aliases, usage hints, and one-line help.
//!
//! Two readers depend on this table: [`super::dispatch`] resolves the typed
//! word through [`Command::resolve`] and then matches on [`Command`], so a
//! registered name without a handler is a compile error; `/help` and the
//! REPL's Tab completion both read [`Command::ALL`], so neither can drift
//! from the dispatcher. They did drift before this table existed — `/help`
//! printed a hardcoded 20 of the 75 dispatchable names.
//!
//! Aliases are a compatibility surface (upstream spellings plus the pinyin
//! aliases). They dispatch, but completion offers and `/help` prints
//! canonical names only.
//!
//! Known limitations: this table is static and enumerates no argument
//! values — the few closed vocabularies that Tab completes live in
//! [`super::super::completion`]. Commands defined by user command files
//! (`.codesmith/commands/*.md`) are loaded at runtime and are not rows here.

/// One dispatchable slash command.
///
/// The variant set is what `dispatch` matches on, exhaustively: adding a
/// name to [`Command::ALL`] without a handler will not compile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Help,
    Exit,
    Clear,
    Status,
    Expand,
    Model,
    Mode,
    Plan,
    Agent,
    Yolo,
    Preset,
    Provider,
    Sessions,
    Resume,
    Save,
    Fork,
    Load,
    Compact,
    Purge,
    New,
    Export,
    Rename,
    Agents,
    Logout,
    Tokens,
    Cost,
    Tools,
    System,
    Context,
    Cache,
    Diff,
    Edit,
    Undo,
    Retry,
    Config,
    Settings,
    Verbose,
    Trust,
    Slop,
    Lsp,
    Theme,
    Statusline,
    Init,
    Hooks,
    Mcp,
    Network,
    Goal,
    Task,
    Jobs,
    Balance,
    Verify,
    Links,
    Home,
    Profile,
    Workspace,
    Translate,
    Anchor,
    Note,
    Attach,
    Feedback,
    Share,
    Skills,
    Skill,
    Review,
    Queue,
    Stash,
    Cycles,
    Relay,
    Restore,
    Change,
    Rlm,
    Extension,
    Mods,
    Models,
    Memory,
    Set,
    Deepseek,
}

/// One row of the registry: a command plus everything the REPL needs to
/// present it.
pub struct CommandInfo {
    pub command: Command,
    /// Canonical name, without the leading `/`.
    pub name: &'static str,
    /// Argument hint printed after the name; empty when the command takes
    /// none.
    pub usage: &'static str,
    /// Accepted compatibility spellings, dispatching to the same handler.
    pub aliases: &'static [&'static str],
    /// One-line description for `/help` and the completion hint.
    pub help: &'static str,
}

impl Command {
    /// Every dispatchable command, in `/help` order.
    pub const ALL: &'static [CommandInfo] = &[
        CommandInfo {
            command: Command::Help,
            name: "help",
            usage: "",
            aliases: &["?"],
            help: "this list",
        },
        CommandInfo {
            command: Command::Exit,
            name: "exit",
            usage: "",
            aliases: &["quit", "q"],
            help: "quit (also Ctrl+D; Ctrl+C twice)",
        },
        CommandInfo {
            command: Command::Clear,
            name: "clear",
            usage: "",
            aliases: &[],
            help: "clear the screen and the tool-detail replay log",
        },
        CommandInfo {
            command: Command::Status,
            name: "status",
            usage: "",
            aliases: &[],
            help: "session status: tokens, cost, model, mode, preset, agents",
        },
        CommandInfo {
            command: Command::Expand,
            name: "expand",
            usage: "[n|all]",
            aliases: &[],
            help: "reprint hidden tool-call detail (Ctrl+O toggles mid-turn)",
        },
        CommandInfo {
            command: Command::Model,
            name: "model",
            usage: "[name|auto]",
            aliases: &[],
            help: "show or switch the model (`auto` re-enables routing)",
        },
        CommandInfo {
            command: Command::Mode,
            name: "mode",
            usage: "[agent|plan|yolo|coordinator]",
            aliases: &[],
            help: "show or switch the mode",
        },
        CommandInfo {
            command: Command::Plan,
            name: "plan",
            usage: "",
            aliases: &[],
            help: "shortcut for /mode plan",
        },
        CommandInfo {
            command: Command::Agent,
            name: "agent",
            usage: "",
            aliases: &[],
            help: "shortcut for /mode agent",
        },
        CommandInfo {
            command: Command::Yolo,
            name: "yolo",
            usage: "",
            aliases: &[],
            help: "shortcut for /mode yolo (shell + trust + auto-approve)",
        },
        CommandInfo {
            command: Command::Preset,
            name: "preset",
            usage: "[name|list|export|off]",
            aliases: &[],
            help: "show, switch, export, or drop the runtime profile tier",
        },
        CommandInfo {
            command: Command::Provider,
            name: "provider",
            usage: "[name]",
            aliases: &[],
            help: "show or switch the API provider",
        },
        CommandInfo {
            command: Command::Sessions,
            name: "sessions",
            usage: "[show|list|prune]",
            aliases: &[],
            help: "list, inspect, or prune saved sessions",
        },
        CommandInfo {
            command: Command::Resume,
            name: "resume",
            usage: "<id|prefix|latest>",
            aliases: &[],
            help: "resume a saved session in place",
        },
        CommandInfo {
            command: Command::Save,
            name: "save",
            usage: "[path]",
            aliases: &[],
            help: "save the session to a file",
        },
        CommandInfo {
            command: Command::Fork,
            name: "fork",
            usage: "",
            aliases: &["branch"],
            help: "save the session and fork it into a sibling",
        },
        CommandInfo {
            command: Command::Load,
            name: "load",
            usage: "<path>",
            aliases: &[],
            help: "load a session file",
        },
        CommandInfo {
            command: Command::Compact,
            name: "compact",
            usage: "[memory|from|up_to]",
            aliases: &["yasuo"],
            help: "compact the context now",
        },
        CommandInfo {
            command: Command::Purge,
            name: "purge",
            usage: "",
            aliases: &["qingchu"],
            help: "delete this session's saved log",
        },
        CommandInfo {
            command: Command::New,
            name: "new",
            usage: "[--force]",
            aliases: &[],
            help: "start a fresh session",
        },
        CommandInfo {
            command: Command::Export,
            name: "export",
            usage: "[path]",
            aliases: &["daochu"],
            help: "export the session transcript",
        },
        CommandInfo {
            command: Command::Rename,
            name: "rename",
            usage: "<title>",
            aliases: &["gaiming"],
            help: "retitle the current session",
        },
        CommandInfo {
            command: Command::Agents,
            name: "agents",
            usage: "",
            aliases: &["subagents"],
            help: "list running sub-agents",
        },
        CommandInfo {
            command: Command::Logout,
            name: "logout",
            usage: "",
            aliases: &[],
            help: "remove the saved API key",
        },
        CommandInfo {
            command: Command::Tokens,
            name: "tokens",
            usage: "",
            aliases: &[],
            help: "last-turn + session token report",
        },
        CommandInfo {
            command: Command::Cost,
            name: "cost",
            usage: "",
            aliases: &[],
            help: "session cost breakdown (parent + sub-agents)",
        },
        CommandInfo {
            command: Command::Tools,
            name: "tools",
            usage: "",
            aliases: &[],
            help: "list the model-visible tool catalog",
        },
        CommandInfo {
            command: Command::System,
            name: "system",
            usage: "",
            aliases: &["xitong"],
            help: "print the current system prompt",
        },
        CommandInfo {
            command: Command::Context,
            name: "context",
            usage: "",
            aliases: &["ctx"],
            help: "context-window snapshot",
        },
        CommandInfo {
            command: Command::Cache,
            name: "cache",
            usage: "[n]",
            aliases: &[],
            help: "per-turn prefix-cache telemetry",
        },
        CommandInfo {
            command: Command::Diff,
            name: "diff",
            usage: "",
            aliases: &[],
            help: "git diff since session start",
        },
        CommandInfo {
            command: Command::Edit,
            name: "edit",
            usage: "",
            aliases: &[],
            help: "reprint the last user message (arrow-up recalls it)",
        },
        CommandInfo {
            command: Command::Undo,
            name: "undo",
            usage: "",
            aliases: &[],
            help: "drop the last exchange",
        },
        CommandInfo {
            command: Command::Retry,
            name: "retry",
            usage: "",
            aliases: &["chongshi"],
            help: "drop the last exchange and re-send the message",
        },
        CommandInfo {
            command: Command::Config,
            name: "config",
            usage: "<key> [value]",
            aliases: &[],
            help: "edit runtime and provider settings",
        },
        CommandInfo {
            command: Command::Settings,
            name: "settings",
            usage: "",
            aliases: &[],
            help: "dump the full persisted settings",
        },
        CommandInfo {
            command: Command::Verbose,
            name: "verbose",
            usage: "[on|off|toggle]",
            aliases: &[],
            help: "toggle verbose thinking detail",
        },
        CommandInfo {
            command: Command::Trust,
            name: "trust",
            usage: "[status|on|off|add|remove]",
            aliases: &["xinren"],
            help: "manage workspace trust",
        },
        CommandInfo {
            command: Command::Slop,
            name: "slop",
            usage: "[query|export]",
            aliases: &["canzha"],
            help: "the slop ledger (query or export)",
        },
        CommandInfo {
            command: Command::Lsp,
            name: "lsp",
            usage: "",
            aliases: &[],
            help: "LSP status",
        },
        CommandInfo {
            command: Command::Theme,
            name: "theme",
            usage: "",
            aliases: &[],
            help: "note: theming was a TUI concept (the REPL uses ANSI colors)",
        },
        CommandInfo {
            command: Command::Statusline,
            name: "statusline",
            usage: "",
            aliases: &[],
            help: "note: the REPL status line is a fixed layout",
        },
        CommandInfo {
            command: Command::Init,
            name: "init",
            usage: "",
            aliases: &[],
            help: "write or update AGENTS.md for this workspace",
        },
        CommandInfo {
            command: Command::Hooks,
            name: "hooks",
            usage: "[list|events]",
            aliases: &["hook", "gouzi"],
            help: "list configured hooks and events",
        },
        CommandInfo {
            command: Command::Mcp,
            name: "mcp",
            usage: "<subcommand>",
            aliases: &[],
            help: "same implementation as `codesmith mcp`",
        },
        CommandInfo {
            command: Command::Network,
            name: "network",
            usage: "[list|allow|deny|remove|default]",
            aliases: &[],
            help: "manage the network allow/deny list",
        },
        CommandInfo {
            command: Command::Goal,
            name: "goal",
            usage: "<quarry>|clear|done|wound|escape",
            aliases: &["hunt", "mubiao"],
            help: "set or inspect this session's goal",
        },
        CommandInfo {
            command: Command::Task,
            name: "task",
            usage: "[add|list|show|cancel]",
            aliases: &["tasks"],
            help: "queue and manage background tasks",
        },
        CommandInfo {
            command: Command::Jobs,
            name: "jobs",
            usage: "",
            aliases: &["job", "zuoye"],
            help: "list running shell jobs",
        },
        CommandInfo {
            command: Command::Balance,
            name: "balance",
            usage: "",
            aliases: &[],
            help: "provider balance availability note",
        },
        CommandInfo {
            command: Command::Verify,
            name: "verify",
            usage: "[stats]",
            aliases: &[],
            help: "verification ledger stats",
        },
        CommandInfo {
            command: Command::Links,
            name: "links",
            usage: "",
            aliases: &["dashboard", "api", "lianjie"],
            help: "provider dashboard and docs links",
        },
        CommandInfo {
            command: Command::Home,
            name: "home",
            usage: "",
            aliases: &["stats", "overview", "zhuye", "shouye"],
            help: "session overview",
        },
        CommandInfo {
            command: Command::Profile,
            name: "profile",
            usage: "<name>",
            aliases: &["dangan"],
            help: "show or switch the local profile",
        },
        CommandInfo {
            command: Command::Workspace,
            name: "workspace",
            usage: "[path]",
            aliases: &["cwd"],
            help: "show the workspace (switching needs a restart)",
        },
        CommandInfo {
            command: Command::Translate,
            name: "translate",
            usage: "",
            aliases: &["translation", "transale"],
            help: "toggle response-locale translation",
        },
        CommandInfo {
            command: Command::Anchor,
            name: "anchor",
            usage: "<text>|list|remove",
            aliases: &["maodian"],
            help: "anchor notes in the transcript",
        },
        CommandInfo {
            command: Command::Note,
            name: "note",
            usage: "<text>|list|show|remove|clear|path",
            aliases: &[],
            help: "session notes",
        },
        CommandInfo {
            command: Command::Attach,
            name: "attach",
            usage: "<path>",
            aliases: &["image", "media", "fujian"],
            help: "queue an image/video for the next message",
        },
        CommandInfo {
            command: Command::Feedback,
            name: "feedback",
            usage: "[category]",
            aliases: &[],
            help: "print the GitHub issue template URLs",
        },
        CommandInfo {
            command: Command::Share,
            name: "share",
            usage: "",
            aliases: &[],
            help: "upload the conversation to a paste service",
        },
        CommandInfo {
            command: Command::Skills,
            name: "skills",
            usage: "[prefix|--remote|sync]",
            aliases: &["jinengliebiao"],
            help: "list local skills, browse the registry, refresh the cache",
        },
        CommandInfo {
            command: Command::Skill,
            name: "skill",
            usage: "<name>|install|update|uninstall",
            aliases: &["jineng"],
            help: "activate a skill or manage installed ones",
        },
        CommandInfo {
            command: Command::Review,
            name: "review",
            usage: "<target>",
            aliases: &["shencha"],
            help: "activate the structured review workflow",
        },
        CommandInfo {
            command: Command::Queue,
            name: "queue",
            usage: "",
            aliases: &["queued"],
            help: "note: the offline queue has no line-mode equivalent yet",
        },
        CommandInfo {
            command: Command::Stash,
            name: "stash",
            usage: "",
            aliases: &["park"],
            help: "note: draft stash has no line-mode equivalent yet",
        },
        CommandInfo {
            command: Command::Cycles,
            name: "cycles",
            usage: "",
            aliases: &["zhouqi", "cycle", "recall"],
            help: "note: cycle briefings are not wired in the REPL yet",
        },
        CommandInfo {
            command: Command::Relay,
            name: "relay",
            usage: "",
            aliases: &["batonpass"],
            help: "note: baton-pass to a teammate is not wired yet",
        },
        CommandInfo {
            command: Command::Restore,
            name: "restore",
            usage: "",
            aliases: &[],
            help: "use /sessions then /resume <id-prefix>",
        },
        CommandInfo {
            command: Command::Change,
            name: "change",
            usage: "",
            aliases: &[],
            help: "changelog: see CHANGELOG.md",
        },
        CommandInfo {
            command: Command::Rlm,
            name: "rlm",
            usage: "",
            aliases: &["recursive", "digui"],
            help: "note: RLM turns run through the model-facing rlm tools",
        },
        CommandInfo {
            command: Command::Extension,
            name: "extension",
            usage: "<sub>",
            aliases: &["ext"],
            help: "list|info|enable|disable|status|reload|install|uninstall",
        },
        CommandInfo {
            command: Command::Mods,
            name: "mods",
            usage: "<sub>",
            aliases: &[],
            help: "list|status|info|activate|enable|disable|remove|reload",
        },
        CommandInfo {
            command: Command::Models,
            name: "models",
            usage: "[--json]",
            aliases: &[],
            help: "list live models from the active endpoint",
        },
        CommandInfo {
            command: Command::Memory,
            name: "memory",
            usage: "[show|path|clear|edit]",
            aliases: &[],
            help: "inspect the memory file (`# text` quick-appends)",
        },
        CommandInfo {
            command: Command::Set,
            name: "set",
            usage: "",
            aliases: &[],
            help: "retired — use /config and /settings",
        },
        CommandInfo {
            command: Command::Deepseek,
            name: "deepseek",
            usage: "",
            aliases: &[],
            help: "renamed — use /links",
        },
    ];

    /// Resolve a typed word (already stripped of the leading `/`) to its
    /// row. Matches canonical names and aliases.
    pub fn resolve(word: &str) -> Option<&'static CommandInfo> {
        Command::ALL
            .iter()
            .find(|info| info.name == word || info.aliases.contains(&word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Tripwire: a variant missing from [`Command::ALL`] would have no name,
    /// no `/help` row, and no completion candidate. Bump this number when
    /// you add a command, and add its row in the same edit.
    #[test]
    fn every_variant_has_a_row() {
        assert_eq!(Command::ALL.len(), 77);
    }

    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen: HashSet<&str> = HashSet::new();
        for info in Command::ALL {
            for word in std::iter::once(info.name).chain(info.aliases.iter().copied()) {
                assert!(seen.insert(word), "duplicate command word {word:?}");
            }
        }
        let total: usize = Command::ALL.iter().map(|info| 1 + info.aliases.len()).sum();
        assert_eq!(seen.len(), total);
    }

    #[test]
    fn resolve_round_trips_names_and_aliases() {
        for info in Command::ALL {
            assert_eq!(
                Command::resolve(info.name).map(|i| i.command),
                Some(info.command),
                "canonical name {:?} does not resolve to itself",
                info.name
            );
            for alias in info.aliases {
                assert_eq!(
                    Command::resolve(alias).map(|i| i.command),
                    Some(info.command),
                    "alias {alias:?} does not resolve to {:?}",
                    info.name
                );
            }
            assert!(!info.help.is_empty(), "{:?} has no help text", info.name);
            assert!(
                !info.usage.starts_with(' '),
                "{:?} usage has padding",
                info.name
            );
        }
        assert!(Command::resolve("plan").is_some());
        assert!(Command::resolve("agent").is_some());
        assert!(Command::resolve("nope").is_none());
        // Dispatch strips the leading `/` before resolving.
        assert!(Command::resolve("/help").is_none());
    }
}
