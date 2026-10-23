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
//! `args` is a command's closed argument vocabulary, completed by Tab and
//! printed by the inline hint. It is empty for "not completed" rather than
//! "takes no arguments": most arguments are paths, session ids, or model
//! names, which the completion callback cannot enumerate. The one dynamic
//! vocabulary (`/provider`, read off [`ApiProvider::all`]) stays in
//! [`super::super::completion`] because it is derived from the provider
//! enum rather than from this table.
//!
//! `group` is the `/help` section; [`Group::ALL`] fixes their order.
//!
//! Known limitations: this table is static. Commands defined by user
//! command files (`.codesmith/commands/*.md`) are loaded at runtime and are
//! not rows here.
//!
//! [`ApiProvider::all`]: crate::config::ApiProvider::all

/// One dispatchable slash command.
///
/// The variant set is what `dispatch` matches on, exhaustively: adding a
/// name here without a handler will not compile.
///
/// The variant set and its enumeration come from one macro list, so they
/// cannot drift: [`Command::all_variants`] is generated from the same names
/// that define the enum. Rust cannot enumerate variants without a macro
/// (there is no stable `variant_count`), and a hand-kept second list would
/// be exactly the staleness this table exists to prevent.
macro_rules! command_table {
    ($($variant:ident),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum Command {
            $($variant),*
        }

        #[cfg(test)]
        impl Command {
            /// Every variant, in declaration order.
            pub(crate) fn all_variants() -> &'static [Command] {
                &[$(Command::$variant),*]
            }
        }
    };
}

command_table! {
    Help, Exit, Clear, Status, Expand, Model, Mode, Plan,
    Agent, Yolo, Preset, Provider, Sessions, Resume, Save, Fork,
    Load, Compact, Purge, New, Export, Rename, Agents, Logout,
    Tokens, Cost, Tools, System, Context, Cache, Diff, Edit,
    Undo, Retry, Config, Settings, Verbose, Trust, Slop, Lsp,
    Theme, Statusline, Init, Hooks, Mcp, Network, Goal, Task,
    Jobs, Balance, Verify, Links, Home, Profile, Workspace, Translate,
    Anchor, Note, Attach, Feedback, Share, Skills, Skill, Review,
    Queue, Stash, Cycles, Relay, Restore, Change, Rlm, Extension,
    Keybindings,
    Effort,
    Autocompact,
    Sandbox,
    ReleaseNotes,
    TerminalSetup,
    Tasks,
    CodeReview,
    SecurityReview,
    Simplify,
    RunApp,
    ClaimCheck,
    Doctor,
    Debug,
    Copy,
    Color,
    Btw,
    Rewind,
    Recap,
    OutputStyle,
    Permissions,
    Cd,
    AddDir,
    Import,
    Insights,
    Pr,
    Loop,
    Branch,
    Subtask,
    Batch,
    FewerPermissionPrompts,
    Stop,
    SkillDoctor,
    RunSkillGenerator,
    UpdateConfig,
    DeepResearch,
    PluginAuthoring,
    ReloadSkills,
    ReloadPlugins,
    Plugins,
    Mods, Models, Memory, Set, Deepseek,
}

/// `/help` section. The order of [`Group::ALL`] is the order `/help` prints
/// them in; every row names exactly one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    /// Starting, saving, resuming, and ending conversations.
    Session,
    /// What the model is carrying: compaction, memory, goals, notes.
    Context,
    /// Model, provider, effort, and the mode ring.
    Model,
    /// Local configuration: settings, trust, sandbox, appearance.
    Config,
    /// Reading and reviewing the work in the tree.
    Code,
    /// Skills, MCP servers, hooks, mods, dylib extensions.
    Extensions,
    /// Health checks and inspection of the running session.
    Diagnostics,
    /// Sharing and reporting.
    Share,
}

impl Group {
    /// Every group, in `/help` order.
    pub const ALL: &'static [Group] = &[
        Group::Session,
        Group::Context,
        Group::Model,
        Group::Config,
        Group::Code,
        Group::Extensions,
        Group::Diagnostics,
        Group::Share,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Group::Session => "session",
            Group::Context => "context & memory",
            Group::Model => "model & mode",
            Group::Config => "configuration",
            Group::Code => "code & review",
            Group::Extensions => "extensions",
            Group::Diagnostics => "diagnostics",
            Group::Share => "share & feedback",
        }
    }
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
    /// Closed argument vocabulary Tab completes. Empty means "not
    /// completed" — see the module note.
    pub args: &'static [&'static str],
    /// Accepted compatibility spellings, dispatching to the same handler.
    pub aliases: &'static [&'static str],
    /// `/help` section this row is printed under.
    pub group: Group,
    /// One-line description for `/help` and the completion hint.
    pub help: &'static str,
}

impl Command {
    /// Every dispatchable command. `/help` regroups these rows through
    /// [`Group::ALL`], so order here is completion-candidate order only.
    pub const ALL: &'static [CommandInfo] = &[
        CommandInfo {
            command: Command::Help,
            name: "help",
            usage: "",
            args: &[],
            aliases: &["?"],
            group: Group::Session,
            help: "this list",
        },
        CommandInfo {
            command: Command::Exit,
            name: "exit",
            usage: "",
            args: &[],
            aliases: &["quit", "q"],
            group: Group::Session,
            help: "quit (also Ctrl+D; Ctrl+C twice)",
        },
        CommandInfo {
            command: Command::Clear,
            name: "clear",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "clear the screen and the tool-detail replay log",
        },
        CommandInfo {
            command: Command::Status,
            name: "status",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "session status: tokens, cost, model, mode, preset, agents",
        },
        CommandInfo {
            command: Command::Expand,
            name: "expand",
            usage: "[n|all]",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "reprint hidden tool-call detail (Ctrl+O toggles mid-turn)",
        },
        CommandInfo {
            command: Command::Model,
            name: "model",
            usage: "[name|auto]",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "show or switch the model (`auto` re-enables routing)",
        },
        CommandInfo {
            command: Command::Mode,
            name: "mode",
            usage: "[agent|plan|yolo|coordinator]",
            args: &["agent", "plan", "yolo", "coordinator"],
            aliases: &[],
            group: Group::Model,
            help: "show or switch the mode",
        },
        CommandInfo {
            command: Command::Plan,
            name: "plan",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "shortcut for /mode plan",
        },
        CommandInfo {
            command: Command::Agent,
            name: "agent",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "shortcut for /mode agent",
        },
        CommandInfo {
            command: Command::Yolo,
            name: "yolo",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "shortcut for /mode yolo (shell + trust + auto-approve)",
        },
        CommandInfo {
            command: Command::Preset,
            name: "preset",
            usage: "[name|list|export|off]",
            args: &["list", "export", "off"],
            aliases: &[],
            group: Group::Model,
            help: "show, switch, export, or drop the runtime profile tier",
        },
        CommandInfo {
            command: Command::Provider,
            name: "provider",
            usage: "[name]",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "show or switch the API provider",
        },
        CommandInfo {
            command: Command::Sessions,
            name: "sessions",
            usage: "[show|list|prune]",
            args: &["show", "list", "prune"],
            aliases: &[],
            group: Group::Session,
            help: "list, inspect, or prune saved sessions",
        },
        CommandInfo {
            command: Command::Resume,
            name: "resume",
            usage: "<id|prefix|latest>",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "resume a saved session in place",
        },
        CommandInfo {
            command: Command::Save,
            name: "save",
            usage: "[path]",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "save the session to a file",
        },
        CommandInfo {
            command: Command::Fork,
            name: "fork",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "save the session and continue in a sibling copy (also /branch)",
        },
        CommandInfo {
            command: Command::Branch,
            name: "branch",
            usage: "[name]",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "branch the conversation here and switch to the copy",
        },
        CommandInfo {
            command: Command::Load,
            name: "load",
            usage: "<path>",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "load a session file",
        },
        CommandInfo {
            command: Command::Compact,
            name: "compact",
            usage: "[memory|from=N|up_to=N]",
            args: &["memory", "from=", "up_to="],
            aliases: &["yasuo"],
            group: Group::Context,
            help: "compact the context now",
        },
        CommandInfo {
            command: Command::Purge,
            name: "purge",
            usage: "",
            args: &[],
            aliases: &["qingchu"],
            group: Group::Session,
            help: "delete this session's saved log",
        },
        CommandInfo {
            command: Command::New,
            name: "new",
            usage: "[--force]",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "start a fresh session",
        },
        CommandInfo {
            command: Command::Export,
            name: "export",
            usage: "[path]",
            args: &[],
            aliases: &["daochu"],
            group: Group::Session,
            help: "export the session transcript",
        },
        CommandInfo {
            command: Command::Rename,
            name: "rename",
            usage: "<title>",
            args: &[],
            aliases: &["gaiming"],
            group: Group::Session,
            help: "retitle the current session",
        },
        CommandInfo {
            command: Command::Agents,
            name: "agents",
            usage: "",
            args: &[],
            aliases: &["subagents"],
            group: Group::Context,
            help: "list running sub-agents",
        },
        CommandInfo {
            command: Command::Subtask,
            name: "subtask",
            usage: "<task>",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "hand a self-contained task to a background sub-agent",
        },
        CommandInfo {
            command: Command::Logout,
            name: "logout",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "remove the saved API key",
        },
        CommandInfo {
            command: Command::Tokens,
            name: "tokens",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "last-turn + session token report",
        },
        CommandInfo {
            command: Command::Cost,
            name: "usage",
            usage: "",
            args: &[],
            aliases: &["cost", "stats"],
            group: Group::Context,
            help: "cost + token report for the session",
        },
        CommandInfo {
            command: Command::Tools,
            name: "tools",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "list the model-visible tool catalog",
        },
        CommandInfo {
            command: Command::System,
            name: "system",
            usage: "",
            args: &[],
            aliases: &["xitong"],
            group: Group::Model,
            help: "print the current system prompt",
        },
        CommandInfo {
            command: Command::Context,
            name: "context",
            usage: "[all]",
            args: &["all"],
            aliases: &["ctx"],
            group: Group::Context,
            help: "context-window snapshot",
        },
        CommandInfo {
            command: Command::Cache,
            name: "cache",
            usage: "[n]",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "per-turn prefix-cache telemetry",
        },
        CommandInfo {
            command: Command::Diff,
            name: "diff",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "git diff since session start",
        },
        CommandInfo {
            command: Command::Edit,
            name: "edit",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "reprint the last user message (arrow-up recalls it)",
        },
        CommandInfo {
            command: Command::Undo,
            name: "undo",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "drop the last exchange",
        },
        CommandInfo {
            command: Command::Retry,
            name: "retry",
            usage: "",
            args: &[],
            aliases: &["chongshi"],
            group: Group::Context,
            help: "drop the last exchange and re-send the message",
        },
        CommandInfo {
            command: Command::Config,
            name: "config",
            usage: "<key> [value]",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "edit runtime and provider settings",
        },
        CommandInfo {
            command: Command::Settings,
            name: "settings",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "dump the full persisted settings",
        },
        CommandInfo {
            command: Command::Verbose,
            name: "verbose",
            usage: "[on|off|toggle]",
            args: &["on", "off", "toggle"],
            aliases: &[],
            group: Group::Model,
            help: "toggle verbose thinking detail",
        },
        CommandInfo {
            command: Command::Trust,
            name: "trust",
            usage: "[status|on|off|add|remove]",
            args: &["status", "on", "off", "add", "remove"],
            aliases: &["xinren"],
            group: Group::Config,
            help: "manage workspace trust",
        },
        CommandInfo {
            command: Command::Effort,
            name: "effort",
            usage: "[low|medium|high|max|auto|off|status]",
            args: &["low", "medium", "high", "max", "auto", "off", "status"],
            aliases: &[],
            group: Group::Model,
            help: "show or set the reasoning-effort tier",
        },
        CommandInfo {
            command: Command::Autocompact,
            name: "autocompact",
            usage: "[auto|<tokens>]",
            args: &["auto"],
            aliases: &[],
            group: Group::Context,
            help: "show or set the auto-compaction window",
        },
        CommandInfo {
            command: Command::Sandbox,
            name: "sandbox",
            usage: "[on|off|status|<mode>]",
            args: &[
                "on",
                "off",
                "status",
                "read-only",
                "workspace-write",
                "danger-full-access",
            ],
            aliases: &[],
            group: Group::Config,
            help: "show or set the command sandbox mode",
        },
        CommandInfo {
            command: Command::ReleaseNotes,
            name: "release-notes",
            usage: "[n|version|latest]",
            args: &["latest"],
            aliases: &[],
            group: Group::Diagnostics,
            help: "the changelog this build carries",
        },
        CommandInfo {
            command: Command::TerminalSetup,
            name: "terminal-setup",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "what to change in your terminal (Shift+Enter, Option as Meta)",
        },
        CommandInfo {
            command: Command::Tasks,
            name: "tasks",
            usage: "",
            args: &[],
            aliases: &["bashes"],
            group: Group::Context,
            help: "everything in the background: jobs, sub-agents, tasks",
        },
        CommandInfo {
            command: Command::Loop,
            name: "loop",
            usage: "[interval] [prompt] | list | run|pause|resume|stop <id>",
            args: &[
                "list", "run", "pause", "resume", "stop", "1h", "2h", "1d", "hourly", "daily",
            ],
            aliases: &["proactive"],
            group: Group::Context,
            help: "run a prompt again and again as a background automation (hourly floor)",
        },
        CommandInfo {
            command: Command::ReloadSkills,
            name: "reload-skills",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Extensions,
            help: "re-read the skill directories",
        },
        CommandInfo {
            command: Command::ReloadPlugins,
            name: "reload-plugins",
            usage: "[--force]",
            args: &["--force"],
            aliases: &[],
            group: Group::Extensions,
            help: "reload mods, extensions, and skills",
        },
        CommandInfo {
            command: Command::Plugins,
            name: "plugins",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Extensions,
            help: "list mods, extensions, and skills together",
        },
        CommandInfo {
            command: Command::CodeReview,
            name: "code-review",
            usage: "[low|medium|high|xhigh|max] [--fix] [target]",
            args: &["low", "medium", "high", "xhigh", "max", "--fix"],
            aliases: &["review"],
            group: Group::Code,
            help: "review the changes for correctness bugs",
        },
        CommandInfo {
            command: Command::SecurityReview,
            name: "security-review",
            usage: "[target]",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "review the changes for security vulnerabilities",
        },
        CommandInfo {
            command: Command::Simplify,
            name: "simplify",
            usage: "[target]",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "review the changed code for simplification",
        },
        CommandInfo {
            command: Command::RunApp,
            name: "run",
            usage: "[target]",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "build and run the project to see the change work",
        },
        CommandInfo {
            command: Command::Pr,
            name: "pr",
            usage: "<number|url> [--repo owner/name] [--checkout]",
            args: &["--checkout", "--repo"],
            aliases: &[],
            group: Group::Code,
            help: "fetch a GitHub PR into the prompt (needs gh)",
        },
        CommandInfo {
            command: Command::FewerPermissionPrompts,
            name: "fewer-permission-prompts",
            usage: "[--yes]",
            args: &["--yes"],
            aliases: &[],
            group: Group::Config,
            help: "propose approval rules from the read-only commands you run most",
        },
        CommandInfo {
            command: Command::Stop,
            name: "stop",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "stop all background work (jobs, sub-agents, tasks)",
        },
        CommandInfo {
            command: Command::SkillDoctor,
            name: "skill-doctor",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Extensions,
            help: "what each skill costs in context and how often it runs",
        },
        CommandInfo {
            command: Command::Batch,
            name: "batch",
            usage: "<instruction>",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "plan a large change as parallel units, one sub-agent each",
        },
        CommandInfo {
            command: Command::RunSkillGenerator,
            name: "run-skill-generator",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "write the project skills that teach /run and /verify how to start it",
        },
        CommandInfo {
            command: Command::UpdateConfig,
            name: "update-config",
            usage: "[request]",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "describe a setting change; the right config file gets edited",
        },
        CommandInfo {
            command: Command::DeepResearch,
            name: "deep-research",
            usage: "<question>",
            args: &[],
            aliases: &[],
            group: Group::Code,
            help: "research a question across sources and cite what it found",
        },
        CommandInfo {
            command: Command::PluginAuthoring,
            name: "plugin-authoring",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Extensions,
            help: "write a mod following docs/MODS.md",
        },
        CommandInfo {
            command: Command::ClaimCheck,
            name: "claim-check",
            usage: "[stats]",
            args: &["stats"],
            aliases: &[],
            group: Group::Diagnostics,
            help: "verification ledger stats (was /verify)",
        },
        CommandInfo {
            command: Command::Doctor,
            name: "doctor",
            usage: "[json]",
            args: &["json"],
            aliases: &["checkup"],
            group: Group::Diagnostics,
            help: "run the setup checks",
        },
        CommandInfo {
            command: Command::Debug,
            name: "debug",
            usage: "[on|off|status]",
            args: &["on", "off", "status"],
            aliases: &[],
            group: Group::Diagnostics,
            help: "verbose diagnostics for this session",
        },
        CommandInfo {
            command: Command::Insights,
            name: "insights",
            usage: "[7d|30d|all]",
            args: &["7d", "30d", "all"],
            aliases: &[],
            group: Group::Diagnostics,
            help: "usage digest plus an HTML report of your sessions",
        },
        CommandInfo {
            command: Command::Copy,
            name: "copy",
            usage: "[N]",
            args: &[],
            aliases: &[],
            group: Group::Share,
            help: "copy the Nth-from-last reply to the clipboard",
        },
        CommandInfo {
            command: Command::Color,
            name: "color",
            usage: "[name|default]",
            args: &[
                "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan", "default",
            ],
            aliases: &[],
            group: Group::Config,
            help: "set the prompt bar's color",
        },
        CommandInfo {
            command: Command::Btw,
            name: "btw",
            usage: "[question]",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "ask about this session without adding to it",
        },
        CommandInfo {
            command: Command::Rewind,
            name: "rewind",
            usage: "[n|list]",
            args: &["list"],
            aliases: &["checkpoint"],
            group: Group::Session,
            help: "list or restore this session's file snapshots",
        },
        CommandInfo {
            command: Command::Recap,
            name: "recap",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Context,
            help: "one-line summary of the session so far",
        },
        CommandInfo {
            command: Command::OutputStyle,
            name: "output-style",
            usage: "[default|concise|status]",
            args: &["default", "concise", "status"],
            aliases: &[],
            group: Group::Config,
            help: "the answer style (is_simple)",
        },
        CommandInfo {
            command: Command::Permissions,
            name: "permissions",
            usage: "[list|allow <prefix>|deny <command>|remove <prefix>]",
            args: &["list", "allow", "deny", "remove"],
            aliases: &["allowed-tools"],
            group: Group::Config,
            help: "what runs without asking, and who decided it",
        },
        CommandInfo {
            command: Command::Cd,
            name: "cd",
            usage: "<path>",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "move the session to another directory (must be trusted)",
        },
        CommandInfo {
            command: Command::AddDir,
            name: "add-dir",
            usage: "<path>",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "trust a directory for this workspace",
        },
        CommandInfo {
            command: Command::Import,
            name: "import",
            usage: "[claude|cursor|codex|gemini] [--dry-run|--yes]",
            args: &["claude", "cursor", "codex", "gemini", "--dry-run", "--yes"],
            aliases: &[],
            group: Group::Extensions,
            help: "bring MCP servers over from another agent CLI (writes only with --yes)",
        },
        CommandInfo {
            command: Command::Keybindings,
            name: "keybindings",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Diagnostics,
            help: "print the keyboard shortcut table (also `?` on an empty prompt)",
        },
        CommandInfo {
            command: Command::Slop,
            name: "slop",
            usage: "[query|export]",
            args: &["query", "export"],
            aliases: &["canzha"],
            group: Group::Diagnostics,
            help: "the slop ledger (query or export)",
        },
        CommandInfo {
            command: Command::Lsp,
            name: "lsp",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "LSP status",
        },
        CommandInfo {
            command: Command::Theme,
            name: "theme",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "the accent theme for prompt and status dot",
        },
        CommandInfo {
            command: Command::Statusline,
            name: "statusline",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "a command whose stdout joins the status line",
        },
        CommandInfo {
            command: Command::Init,
            name: "init",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "write or update AGENTS.md for this workspace",
        },
        CommandInfo {
            command: Command::Hooks,
            name: "hooks",
            usage: "[list|events]",
            args: &["list", "events"],
            aliases: &["hook", "gouzi"],
            group: Group::Extensions,
            help: "list configured hooks and events",
        },
        CommandInfo {
            command: Command::Mcp,
            name: "mcp",
            usage: "<subcommand>",
            args: &[
                "list", "connect", "auth", "tools", "add", "remove", "enable", "disable",
                "validate",
            ],
            aliases: &[],
            group: Group::Extensions,
            help: "same implementation as `codesmith mcp`",
        },
        CommandInfo {
            command: Command::Network,
            name: "network",
            usage: "[list|allow|deny|remove|default]",
            args: &["list", "allow", "deny", "remove", "default"],
            aliases: &[],
            group: Group::Config,
            help: "manage the network allow/deny list",
        },
        CommandInfo {
            command: Command::Goal,
            name: "goal",
            usage: "<quarry>|clear|done|wound|escape",
            args: &["clear", "done", "wound", "escape"],
            aliases: &["hunt", "mubiao"],
            group: Group::Context,
            help: "set or inspect this session's goal",
        },
        CommandInfo {
            command: Command::Task,
            name: "task",
            usage: "[add|list|show|cancel]",
            args: &["add", "list", "show", "cancel"],
            aliases: &[],
            group: Group::Context,
            help: "queue and manage background tasks",
        },
        CommandInfo {
            command: Command::Jobs,
            name: "jobs",
            usage: "",
            args: &[],
            aliases: &["job", "zuoye"],
            group: Group::Context,
            help: "list running shell jobs",
        },
        CommandInfo {
            command: Command::Balance,
            name: "balance",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "provider balance availability note",
        },
        CommandInfo {
            command: Command::Verify,
            name: "verify",
            usage: "[target]",
            args: &[],
            aliases: &[],
            group: Group::Diagnostics,
            help: "activate the verify-app skill (ledger stats moved to /claim-check)",
        },
        CommandInfo {
            command: Command::Links,
            name: "links",
            usage: "",
            args: &[],
            aliases: &["dashboard", "api", "lianjie"],
            group: Group::Model,
            help: "provider dashboard and docs links",
        },
        CommandInfo {
            command: Command::Home,
            name: "home",
            usage: "",
            args: &[],
            aliases: &["overview", "zhuye", "shouye"],
            group: Group::Session,
            help: "session overview",
        },
        CommandInfo {
            command: Command::Profile,
            name: "profile",
            usage: "<name>",
            args: &[],
            aliases: &["dangan"],
            group: Group::Session,
            help: "show or switch the local profile",
        },
        CommandInfo {
            command: Command::Workspace,
            name: "workspace",
            usage: "[path]",
            args: &[],
            aliases: &["cwd"],
            group: Group::Session,
            help: "show the workspace (switch live via /cd)",
        },
        CommandInfo {
            command: Command::Translate,
            name: "translate",
            usage: "",
            args: &[],
            // "transale" is kept as a common misspelling of "translate".
            aliases: &["translation", "transale"],
            group: Group::Model,
            help: "toggle response-locale translation",
        },
        CommandInfo {
            command: Command::Anchor,
            name: "anchor",
            usage: "<text>|list|remove",
            args: &["list", "remove"],
            aliases: &["maodian"],
            group: Group::Context,
            help: "anchor notes in the transcript",
        },
        CommandInfo {
            command: Command::Note,
            name: "note",
            usage: "<text>|list|show|remove|clear|path",
            args: &["list", "show", "remove", "clear", "path"],
            aliases: &[],
            group: Group::Context,
            help: "session notes",
        },
        CommandInfo {
            command: Command::Attach,
            name: "attach",
            usage: "<path>",
            args: &[],
            aliases: &["image", "media", "fujian"],
            group: Group::Context,
            help: "queue an image/video for the next message",
        },
        CommandInfo {
            command: Command::Feedback,
            name: "feedback",
            usage: "up|down [N] [comment]",
            args: &["up", "down"],
            aliases: &[],
            group: Group::Share,
            help: "rate the Nth-from-last reply; recorded locally, never shown to the model",
        },
        CommandInfo {
            command: Command::Share,
            name: "share",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Share,
            help: "upload the conversation to a paste service",
        },
        CommandInfo {
            command: Command::Skills,
            name: "skills",
            usage: "[prefix|--remote|sync]",
            args: &["--remote", "sync"],
            aliases: &["jinengliebiao"],
            group: Group::Extensions,
            help: "list local skills, browse the registry, refresh the cache",
        },
        CommandInfo {
            command: Command::Skill,
            name: "skill",
            usage: "<name>|install|update|uninstall|trust",
            args: &["install", "update", "uninstall", "trust"],
            aliases: &["jineng"],
            group: Group::Extensions,
            help: "activate a skill or manage installed ones",
        },
        CommandInfo {
            command: Command::Review,
            name: "review-skill",
            usage: "<target>",
            args: &[],
            aliases: &["shencha"],
            group: Group::Extensions,
            help: "activate the user-installed review skill (was /review)",
        },
        CommandInfo {
            command: Command::Queue,
            name: "queue",
            usage: "",
            args: &[],
            aliases: &["queued"],
            group: Group::Context,
            help: "messages queued while a turn was running (Up takes them back)",
        },
        CommandInfo {
            command: Command::Stash,
            name: "stash",
            usage: "",
            args: &[],
            aliases: &["park"],
            group: Group::Context,
            help: "the draft stash is Ctrl+S, at the prompt",
        },
        CommandInfo {
            command: Command::Cycles,
            name: "cycles",
            usage: "",
            args: &[],
            aliases: &["zhouqi", "cycle", "recall"],
            group: Group::Context,
            help: "note: cycle briefings are not wired in the REPL yet",
        },
        CommandInfo {
            command: Command::Relay,
            name: "relay",
            usage: "",
            args: &[],
            aliases: &["batonpass"],
            group: Group::Context,
            help: "note: baton-pass to a teammate is not wired yet",
        },
        CommandInfo {
            command: Command::Restore,
            name: "restore",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "use /sessions then /resume <id-prefix>",
        },
        CommandInfo {
            command: Command::Change,
            name: "change",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Session,
            help: "changelog: see CHANGELOG.md",
        },
        CommandInfo {
            command: Command::Rlm,
            name: "rlm",
            usage: "",
            args: &[],
            aliases: &["recursive", "digui"],
            group: Group::Context,
            help: "note: RLM turns run through the model-facing rlm tools",
        },
        CommandInfo {
            command: Command::Extension,
            name: "extension",
            usage: "<sub>",
            args: &[
                "list",
                "info",
                "enable",
                "disable",
                "status",
                "reload",
                "install",
                "uninstall",
            ],
            aliases: &["ext"],
            group: Group::Extensions,
            help: "list|info|enable|disable|status|reload|install|uninstall",
        },
        CommandInfo {
            command: Command::Mods,
            name: "mods",
            usage: "<sub>",
            args: &[
                "list", "status", "info", "activate", "enable", "disable", "remove", "reload",
            ],
            aliases: &[],
            group: Group::Extensions,
            help: "list|status|info|activate|enable|disable|remove|reload",
        },
        CommandInfo {
            command: Command::Models,
            name: "models",
            usage: "[--json]",
            args: &[],
            aliases: &[],
            group: Group::Model,
            help: "list live models from the active endpoint",
        },
        CommandInfo {
            command: Command::Memory,
            name: "memory",
            usage: "[show|path|clear|edit]",
            args: &["show", "path", "clear", "edit"],
            aliases: &[],
            group: Group::Context,
            help: "inspect the memory file (`# text` quick-appends)",
        },
        CommandInfo {
            command: Command::Set,
            name: "set",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Config,
            help: "retired — use /config and /settings",
        },
        CommandInfo {
            command: Command::Deepseek,
            name: "deepseek",
            usage: "",
            args: &[],
            aliases: &[],
            group: Group::Model,
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
    /// no `/help` row, no completion candidate, and no argument vocabulary —
    /// while `dispatch` (whose `match` is exhaustive) would still handle it.
    /// The reference set comes from the macro list that defines the enum, so
    /// it cannot go stale; this replaces an `ALL.len() == 77` assertion whose
    /// number had to be remembered and whose failure named no command.
    #[test]
    fn every_variant_has_a_row() {
        let registered: HashSet<Command> = Command::ALL.iter().map(|info| info.command).collect();
        let mut missing = Vec::new();
        for variant in Command::all_variants() {
            if !registered.contains(variant) {
                missing.push(format!("{variant:?}"));
            }
        }
        assert!(
            missing.is_empty(),
            "no row in Command::ALL for: {}",
            missing.join(", ")
        );
        assert_eq!(
            Command::ALL.len(),
            Command::all_variants().len(),
            "Command::ALL has rows the enum does not have variants for"
        );
    }

    /// Tripwire: `/help` prints sections by iterating [`Group::ALL`], so a
    /// row whose group is not listed there vanishes from the help output.
    #[test]
    fn every_row_group_is_listed() {
        for info in Command::ALL {
            assert!(
                Group::ALL.contains(&info.group),
                "/{} uses a group missing from Group::ALL and would not appear in /help",
                info.name
            );
        }
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
