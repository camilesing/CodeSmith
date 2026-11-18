//! Preset & config center — the desktop UI's window onto preset tiers and
//! the full `config.toml`. Exposed over `codesmith serve --http` as
//! `GET /v1/presets` (snapshot) and `POST /v1/presets/apply` (edits); the
//! Tauri bridge forwards both as `agentPresets/describe|apply`.
//!
//! Model: [`snapshot`] describes the *running* fold (`state.config`) plus
//! the *file* layer, and derives per-key provenance without re-running the
//! load pipeline: a key present in the file is `explicit-file` (the only
//! kind the UI may unset); a governed key whose effective value equals the
//! selected tier's baseline is `baseline`; anything else with a value is
//! `explicit-other` (env / CLI flag / project overlay — visible, not
//! editable-away); the rest are `default`.
//!
//! Writes are surgical `toml_edit` operations validated by deserializing
//! the whole document into [`crate::config::Config`] before persisting, so
//! the file can never be written invalid, and comments/unknown keys always
//! survive.
//!
//! Known limitations (product decision 2026-11-18):
//! - Edits take effect when the serving process next folds config
//!   (restart); running sessions are unaffected. `restartPending` says so.
//! - No cross-window CAS or push — last writer wins (file writes are
//!   atomic).
//! - The curated catalog is addable from the UI; arbitrary *new* keys are
//!   not (only leaf values already present in the file may be edited
//!   generically — whole-table set/unset is refused so a section's keys
//!   cannot be destroyed), so the UI cannot conjure typo keys. `app_mode`
//!   and `memory_level` are preset-file/session dials, not config.toml
//!   keys, and are not cataloged.
//! - The REPL's `/preset` persists to settings.toml, which the `preset`
//!   key written here outranks — switching tiers from the desktop masks
//!   the REPL selection until the key is unset.

use std::path::Path;

use codesmith_config::config_edit::{doc_set, doc_unset};
use codesmith_config::presets::{
    MemoryLevel, PresetCatalog, PresetDefinitionToml, canonicalize_preset_name,
};
use codesmith_config::write_config_file_private;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use toml_edit::DocumentMut;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Control shape of a catalog key — drives both the UI widget and write
/// validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Enum {
        options: &'static [&'static str],
    },
    Bool,
    String,
    Secret,
    Int,
    StringList,
    StringMap,
    /// Whole-table passthrough: rendered read-only (dedicated editors own it).
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyGroup {
    Session,
    Safety,
    Tools,
    Memory,
    Features,
    General,
}

#[derive(Clone)]
pub struct CatalogKey {
    pub path: &'static str,
    pub kind: KeyKind,
    pub group: KeyGroup,
    /// Baselined by the selected preset (fill-if-unset; deviation ⇒ diy).
    pub preset_governed: bool,
    pub read_only: bool,
    pub label_en: &'static str,
    pub label_zh: &'static str,
    pub desc_en: &'static str,
    pub desc_zh: &'static str,
}

const EFFORTS: &[&str] = &["off", "low", "medium", "high", "max", "auto"];
const APPROVALS: &[&str] = &[
    "suggest",
    "suggested",
    "on-request",
    "untrusted",
    "auto",
    "never",
];
const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
const NET_DECISIONS: &[&str] = &["allow", "deny", "prompt"];

macro_rules! keys {
    ($( $path:literal, $kind:expr, $group:ident, $gov:literal, $ro:literal,
        $len:literal, $lzh:literal, $den:literal, $dzh:literal );* $(;)?) => {
        &[ $( CatalogKey {
            path: $path, kind: $kind, group: KeyGroup::$group,
            preset_governed: $gov, read_only: $ro,
            label_en: $len, label_zh: $lzh, desc_en: $den, desc_zh: $dzh,
        } ),* ]
    };
}

/// Known feature flags (`codesmith_agent_runtime::features::FEATURES` keys).
const FEATURE_KEYS: &[&str] = &[
    "shell_tool",
    "subagents",
    "web_search",
    "apply_patch",
    "mcp",
    "exec_policy",
    "vision_model",
    "knowledge_on_demand",
    "agent_teams",
    "coordinator_mode",
    "file_freshness",
];

pub fn catalog() -> &'static [CatalogKey] {
    static CATALOG: &[CatalogKey] = keys! {
        // -- session & model ------------------------------------------------
        "provider", KeyKind::String, Session, true, false,
        "Provider", "提供商",
        "Model provider the sessions run on", "会话使用的模型提供商";
        "model", KeyKind::String, Session, false, false,
        "Model", "模型",
        "Main chat model id", "主对话模型 id";
        "default_text_model", KeyKind::String, Session, false, false,
        "Default text model", "默认文本模型",
        "Fallback model id when none is selected", "未选择模型时的回退 id";
        "custom_provider", KeyKind::String, Session, false, false,
        "Custom provider", "自定义提供商",
        "Active custom provider id (overrides provider)", "生效的自定义提供商 id（优先于 provider）";
        "turn_executor", KeyKind::String, Session, false, false,
        "Turn executor", "回合执行器",
        "Main-turn executor id (builtin or a registered factory)", "主回合执行器 id（builtin 或已注册工厂）";
        "reasoning_effort", KeyKind::Enum { options: EFFORTS }, Session, true, false,
        "Reasoning effort", "推理力度",
        "Model reasoning-effort tier", "模型推理力度档位";
        "max_subagents", KeyKind::Int, Session, true, false,
        "Max subagents", "子代理上限",
        "Maximum concurrent in-process subagents", "进程内子代理并发上限";
        "auth_mode", KeyKind::String, Session, false, false,
        "Auth mode", "认证模式",
        "Credential auth mode", "凭据认证模式";
        "output_mode", KeyKind::String, Session, false, false,
        "Output mode", "输出模式",
        "Headless output verbosity", "无头模式输出详细度";
        "log_level", KeyKind::String, Session, false, false,
        "Log level", "日志级别",
        "Log filter level", "日志过滤级别";

        // -- safety ----------------------------------------------------------
        "approval_policy", KeyKind::Enum { options: APPROVALS }, Safety, true, false,
        "Approval policy", "审批策略",
        "When tool calls need user approval", "工具调用何时需要用户审批";
        "sandbox_mode", KeyKind::Enum { options: SANDBOX_MODES }, Safety, true, false,
        "Sandbox mode", "沙箱模式",
        "Launch sandbox policy (per-session presets may tighten)", "启动沙箱策略（每会话可再收紧）";
        "strict_tool_mode", KeyKind::Bool, Safety, true, false,
        "Strict tool mode", "严格工具模式",
        "Force tool_choice and strict function schemas", "强制 tool_choice 与严格函数 schema";
        "network.audit", KeyKind::Bool, Safety, true, false,
        "Network audit", "网络审计",
        "Log one audit line per outbound network call", "每次外联记录一行审计日志";
        "network.default", KeyKind::Enum { options: NET_DECISIONS }, Safety, false, false,
        "Network default", "网络默认决策",
        "Decision for hosts not in allow/deny", "不在允许/拒绝名单中的主机的决策";
        "network.allow", KeyKind::StringList, Safety, false, false,
        "Network allow", "网络允许名单",
        "Always-allowed hosts (.example.com matches subdomains)", "始终允许的主机（.example.com 匹配子域）";
        "network.deny", KeyKind::StringList, Safety, false, false,
        "Network deny", "网络拒绝名单",
        "Always-denied hosts (wins over allow)", "始终拒绝的主机（优先于允许）";
        "network.proxy", KeyKind::StringList, Safety, false, false,
        "Network proxy hosts", "网络代理主机",
        "Hostnames allowed to resolve to trusted proxy ranges", "允许解析到可信代理网段的主机名";
        "sandbox.enabled", KeyKind::Bool, Safety, false, false,
        "Sandbox enabled", "启用沙箱",
        "Structured sandbox master switch", "结构化沙箱总开关";
        "sandbox.fail_if_unavailable", KeyKind::Bool, Safety, false, false,
        "Fail if unavailable", "沙箱不可用即失败",
        "Refuse to run unsandboxed when the backend is missing", "后端缺失时拒绝无沙箱运行";
        "sandbox.enabled_platforms", KeyKind::StringList, Safety, false, false,
        "Sandbox platforms", "沙箱平台",
        "Platforms the sandbox applies on", "启用沙箱的平台";
        "sandbox.excluded_commands", KeyKind::StringList, Safety, false, false,
        "Excluded commands", "豁免命令",
        "Commands that skip the sandbox", "跳过沙箱的命令";
        "sandbox.auto_allow_bash_if_sandboxed", KeyKind::Bool, Safety, false, false,
        "Auto-allow under sandbox", "沙箱下自动放行",
        "Auto-allow shell when sandboxed", "沙箱生效时自动放行 shell";
        "sandbox.auto_allow", KeyKind::StringList, Safety, false, false,
        "Auto-allow prefixes", "自动放行前缀",
        "Shell prefixes that never prompt", "不再提示审批的命令前缀";
        "sandbox.prefer_bwrap", KeyKind::Bool, Safety, false, false,
        "Prefer bubblewrap", "优先 bubblewrap",
        "Route exec through bwrap on Linux when present", "Linux 上存在 bwrap 时走 bwrap";
        "sandbox.filesystem.mode", KeyKind::String, Safety, false, false,
        "Filesystem mode", "文件系统模式",
        "Filesystem sandbox policy mode", "文件系统沙箱策略模式";
        "sandbox.filesystem.writable_roots", KeyKind::StringList, Safety, false, false,
        "Writable roots", "可写根",
        "Roots writable inside the sandbox", "沙箱内可写的根路径";
        "sandbox.filesystem.allow_read", KeyKind::StringList, Safety, false, false,
        "Allow read", "允许读取",
        "Extra readable paths", "额外可读路径";
        "sandbox.filesystem.deny_read", KeyKind::StringList, Safety, false, false,
        "Deny read", "拒绝读取",
        "Unreadable paths", "禁止读取的路径";
        "sandbox.filesystem.allow_write", KeyKind::StringList, Safety, false, false,
        "Allow write", "允许写入",
        "Extra writable paths", "额外可写路径";
        "sandbox.filesystem.deny_write", KeyKind::StringList, Safety, false, false,
        "Deny write", "拒绝写入",
        "Unwritable paths", "禁止写入的路径";
        "sandbox.filesystem.exclude_tmpdir", KeyKind::Bool, Safety, false, false,
        "Exclude tmpdir", "排除临时目录",
        "Keep the OS tmpdir out of the sandbox map", "沙箱映射排除系统临时目录";
        "sandbox.filesystem.exclude_slash_tmp", KeyKind::Bool, Safety, false, false,
        "Exclude /tmp", "排除 /tmp",
        "Keep /tmp out of the sandbox map", "沙箱映射排除 /tmp";
        "sandbox.network.enabled", KeyKind::Bool, Safety, false, false,
        "Sandbox network gate", "沙箱网络门",
        "Gate outbound network inside the sandbox", "沙箱内管控外联";
        "sandbox.network.allow_managed_domains_only", KeyKind::Bool, Safety, false, false,
        "Managed domains only", "仅托管域名",
        "Allow only managed domains through the gate", "仅放行托管域名";
        "sandbox.network.allow", KeyKind::StringList, Safety, false, false,
        "Sandbox network allow", "沙箱网络允许",
        "Hosts allowed from the sandbox", "沙箱内允许访问的主机";
        "sandbox.network.deny", KeyKind::StringList, Safety, false, false,
        "Sandbox network deny", "沙箱网络拒绝",
        "Hosts denied from the sandbox", "沙箱内拒绝访问的主机";
        "sandbox_backend", KeyKind::String, Safety, false, false,
        "Sandbox backend", "沙箱后端",
        "External sandbox backend (none | opensandbox)", "外部沙箱后端（none | opensandbox）";
        "sandbox_url", KeyKind::String, Safety, false, false,
        "Sandbox backend URL", "沙箱后端地址",
        "External sandbox backend base URL", "外部沙箱后端 base URL";
        "sandbox_api_key", KeyKind::Secret, Safety, false, false,
        "Sandbox backend key", "沙箱后端密钥",
        "Bearer token for the sandbox backend", "沙箱后端的 Bearer token";

        // -- tools -------------------------------------------------------------
        "tools.progressive_disclosure", KeyKind::Bool, Tools, true, false,
        "Progressive disclosure", "工具渐进披露",
        "Withhold deferred tools, offer via tool_search", "隐藏可延迟工具，按需经 tool_search 提供";
        "tools.always_load", KeyKind::StringList, Tools, false, false,
        "Always-load tools", "常驻工具",
        "Native tools kept loaded outside the core catalog", "核心目录之外常驻加载的原生工具";

        // -- memory & context ---------------------------------------------------
        "memory.enabled", KeyKind::Bool, Memory, true, false,
        "Memory", "持久记忆",
        "Persistent user-level memory", "用户级持久记忆";
        "memory.kod_enabled", KeyKind::Bool, Memory, true, false,
        "Knowledge on demand", "知识按需召回",
        "Kod recall over the memory store", "对记忆库的 kod 召回";
        "context.enabled", KeyKind::Bool, Memory, true, false,
        "Layered context", "分层上下文",
        "Append-only layered context management", "只增分层上下文管理";
        "context.project_pack", KeyKind::Bool, Memory, true, false,
        "Project pack", "项目包",
        "Include the project context pack", "纳入项目上下文包";
        "index.enabled", KeyKind::Bool, Memory, true, false,
        "Code index", "代码索引",
        "Persistent tree-sitter symbol index", "持久 tree-sitter 符号索引";
        "lsp.enabled", KeyKind::Bool, Memory, true, false,
        "LSP diagnostics", "LSP 诊断",
        "Post-edit LSP diagnostics injection", "编辑后注入 LSP 诊断";
        "lsp.include_warnings", KeyKind::Bool, Memory, true, false,
        "LSP warnings", "LSP 警告",
        "Include warning-level diagnostics", "包含 warning 级诊断";
        "snapshots.enabled", KeyKind::Bool, Memory, true, false,
        "Workspace snapshots", "工作区快照",
        "Side-git snapshot per turn", "每回合 side-git 快照";
        "capacity.enabled", KeyKind::Bool, Memory, true, false,
        "Capacity controller", "容量控制器",
        "Capacity-decision analytics and gates", "容量决策分析与门控";
        "auto.cost_saving", KeyKind::Bool, Memory, true, false,
        "Auto cost saving", "自动省成本",
        "Cost-saving bias for --model auto", "--model auto 的省成本倾向";

        // -- features (generated) ---------------------------------------------
        // Placeholder: FEATURE_KEYS rows are appended by `catalog_keys()`.
        // (macro table above stays static; features join below.)

        // -- general -------------------------------------------------------------
        "preset", KeyKind::String, General, false, true,
        "Preset", "预设档位",
        "Selected tier — switch via the tier cards above", "当前档位——用上方档位卡片切换";
        "api_key", KeyKind::Secret, General, false, false,
        "API key", "API 密钥",
        "Root-level provider API key", "根级提供商 API 密钥";
        "base_url", KeyKind::String, General, false, false,
        "Base URL", "服务地址",
        "Root-level API base URL", "根级 API base URL";
        "http_headers", KeyKind::StringMap, General, false, false,
        "HTTP headers", "HTTP 头",
        "Extra headers on model API requests", "模型 API 请求的附加头";
        "telemetry", KeyKind::Bool, General, false, false,
        "Telemetry", "本地遥测",
        "Local-only telemetry events (default off)", "仅本地遥测事件（默认关闭）";
        "compaction.strategy", KeyKind::String, General, false, false,
        "Compaction strategy", "压缩策略",
        "builtin or a mod-registered strategy", "builtin 或 mod 注册的策略";
        "snapshots.max_age_days", KeyKind::Int, General, false, false,
        "Snapshot retention", "快照保留天数",
        "Prune snapshots older than this", "清理早于该天数的快照";
        "edit.parse_gate", KeyKind::Bool, General, false, false,
        "Parse gate", "解析门",
        "Reject writes that break parsing files", "拒绝会破坏文件可解析性的写入";
        "skills.registry_url", KeyKind::String, General, false, false,
        "Skills registry", "技能注册表",
        "Community skill registry index URL", "社区技能注册表索引 URL";
        "skills.max_install_size_bytes", KeyKind::Int, General, false, false,
        "Skill size cap", "技能大小上限",
        "Max uncompressed skill size in bytes", "技能解压后最大字节数";
        "lsp.poll_after_edit_ms", KeyKind::Int, General, false, false,
        "LSP poll delay", "LSP 轮询延迟",
        "Milliseconds to wait for diagnostics after an edit", "编辑后等待诊断的毫秒数";
        "hook_sinks.unix_socket_path", KeyKind::String, General, false, false,
        "Hook sink socket", "钩子汇 socket",
        "Unix socket path for the app-server event sink", "app-server 事件汇的 Unix socket 路径";
        "desktop.enabled", KeyKind::Bool, General, false, false,
        "Desktop UI", "桌面界面",
        "Launch the desktop app on preset all", "preset 为 all 时启动桌面应用";
        "rhai_loop", KeyKind::Table, General, false, true,
        "Rhai loop", "Rhai 循环",
        "Script-loop settings (advanced)", "脚本循环设置（高级）";
        "providers", KeyKind::Table, General, false, true,
        "Providers", "提供商表",
        "Edit provider endpoints on the Models page", "在 Models 页编辑提供商端点";
    };
    // The static table plus the generated feature rows, joined once.
    static JOINED: std::sync::OnceLock<Vec<CatalogKey>> = std::sync::OnceLock::new();
    JOINED
        .get_or_init(|| {
            let mut all: Vec<CatalogKey> = CATALOG.to_vec();
            for key in FEATURE_KEYS {
                all.push(CatalogKey {
                    path: Box::leak(format!("features.{key}").into_boxed_str()),
                    kind: KeyKind::Bool,
                    group: KeyGroup::Features,
                    preset_governed: true,
                    read_only: false,
                    label_en: key,
                    label_zh: key,
                    desc_en: "Feature flag (preset-governed baseline)",
                    desc_zh: "功能开关（preset 管辖基线）",
                });
            }
            all
        })
        .as_slice()
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TierView {
    pub name: String,
    pub description: String,
    /// builtin | user | project
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeySource {
    ExplicitFile,
    ExplicitOther,
    Baseline,
    Default,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineView {
    pub tier: String,
    pub value: Json,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStateView {
    pub path: String,
    pub group: KeyGroup,
    pub preset_governed: bool,
    pub read_only: bool,
    pub secret: bool,
    pub kind: &'static str,
    pub options: Option<&'static [&'static str]>,
    pub label_en: String,
    pub label_zh: String,
    pub desc_en: String,
    pub desc_zh: String,
    /// Effective value in the running process (masked for secrets: null +
    /// `set`).
    pub value: Json,
    /// Whether a secret/path currently holds a value.
    pub set: bool,
    pub source: KeySource,
    pub baselines: Vec<BaselineView>,
    /// UI may remove the explicit override (explicit-file, not read-only).
    pub resettable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionView {
    pub selected: String,
    /// builtin | config | settings | default — resolved like startup does.
    pub selected_source: String,
    /// The tier name, or `diy` when explicit config deviates.
    pub effective: String,
    /// Governed keys whose explicit file value differs from the baseline.
    pub deviated: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetSnapshot {
    pub tiers: Vec<TierView>,
    pub selection: SelectionView,
    pub keys: Vec<KeyStateView>,
    /// Raw user config.toml as a JSON tree (unknown keys visible, secrets
    /// masked) — the generic editor's data.
    pub file_tree: Json,
    pub restart_pending: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum PresetOp {
    SelectPreset { name: String },
    Set { path: String, value: Json },
    Unset { path: String },
}

#[derive(Debug)]
pub struct ApplyError {
    pub code: &'static str,
    pub message: String,
}

impl ApplyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Read the config file, treating an absent file as empty. Any other IO
/// error (permissions, EIO, a directory at the path) fails loud: silently
/// proceeding would present a phantom empty file — and in `apply`, write
/// ops applied to that phantom back over the real one.
fn read_config_or_empty(config_path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(config_path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

fn edit_to_json(item: &toml_edit::Item, secret: bool) -> Json {
    use toml_edit::Item;
    match item {
        Item::None | Item::Table(_) | Item::ArrayOfTables(_) => Json::Null,
        Item::Value(value) => value_to_json(value, secret),
    }
}

fn value_to_json(value: &toml_edit::Value, secret: bool) -> Json {
    use toml_edit::Value;
    match value {
        Value::String(s) => {
            if secret {
                Json::String("•••".to_string())
            } else {
                Json::String(s.value().to_string())
            }
        }
        Value::Integer(i) => Json::Number((*i.value()).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Boolean(b) => Json::Bool(*b.value()),
        Value::Datetime(d) => Json::String(d.to_string()),
        Value::Array(items) => Json::Array(items.iter().map(|v| value_to_json(v, false)).collect()),
        Value::InlineTable(table) => {
            let mut map = serde_json::Map::new();
            for (key, val) in table.iter() {
                map.insert(key.to_string(), value_to_json(val, false));
            }
            Json::Object(map)
        }
    }
}

/// Where a dotted path's walk ended: a regular item, or a value inside an
/// inline table (which has items of type `Value`, not `Item`).
enum Walked<'a> {
    Item(&'a toml_edit::Item),
    Inline(&'a toml_edit::Value),
}

/// Look a dotted path up in a parsed document, descending both real tables
/// and inline tables.
fn doc_walk<'a>(doc: &'a DocumentMut, path: &str) -> Option<Walked<'a>> {
    fn walk_table<'a>(table: &'a toml_edit::Table, segments: &[&str]) -> Option<Walked<'a>> {
        let (head, rest) = segments.split_first()?;
        match table.get(head)? {
            // `rest.is_empty()` first: a path may legitimately end on a
            // table — returning it (not recursing into nothing) is what
            // lets whole-table targets be seen and refused by `apply`.
            item if rest.is_empty() => Some(Walked::Item(item)),
            toml_edit::Item::Table(nested) => walk_table(nested, rest),
            toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)) => {
                walk_inline(inline, rest)
            }
            _ => None,
        }
    }
    fn walk_inline<'a>(
        inline: &'a toml_edit::InlineTable,
        segments: &[&str],
    ) -> Option<Walked<'a>> {
        let (head, rest) = segments.split_first()?;
        let value = inline.get(head)?;
        match value {
            v if rest.is_empty() => Some(Walked::Inline(v)),
            toml_edit::Value::InlineTable(nested) => walk_inline(nested, rest),
            _ => None,
        }
    }
    let segments: Vec<&str> = path.split('.').collect();
    walk_table(doc.as_table(), &segments)
}

/// JSON view of a dotted path in the document (unmasked — callers decide).
fn doc_lookup_json(doc: &DocumentMut, path: &str) -> Option<Json> {
    match doc_walk(doc, path)? {
        Walked::Item(toml_edit::Item::None) => None,
        Walked::Item(toml_edit::Item::Table(table)) => Some(table_to_json(table, path)),
        Walked::Item(other) => Some(edit_to_json(other, false)),
        Walked::Inline(value) => Some(value_to_json(value, false)),
    }
}

/// Whether a dotted path exists in the document.
fn doc_has(doc: &DocumentMut, path: &str) -> bool {
    doc_walk(doc, path).is_some()
}

/// Refuse ops whose target addresses a whole table (real table or array of
/// tables): a set would overwrite every key under it with one value, an
/// unset would drop them all. Cataloged tables (`rhai_loop`, `providers`)
/// and unknown sections alike — generic edits and resets touch leaves only.
fn refuse_table_target(doc: &DocumentMut, path: &str) -> Result<(), ApplyError> {
    if matches!(
        doc_walk(doc, path),
        Some(Walked::Item(toml_edit::Item::Table(_)))
            | Some(Walked::Item(toml_edit::Item::ArrayOfTables(_)))
    ) {
        return Err(ApplyError::new(
            "read_only",
            format!("{path:?} addresses a whole table; edit its keys individually"),
        ));
    }
    Ok(())
}

/// Render a document table as a JSON tree for the generic editor, masking
/// secret paths (path-aware so nested secrets mask too).
fn table_to_json(table: &toml_edit::Table, prefix: &str) -> Json {
    let mut map = serde_json::Map::new();
    for (key, item) in table.iter() {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        let value = match item {
            toml_edit::Item::None => continue,
            toml_edit::Item::Table(nested) => table_to_json(nested, &path),
            toml_edit::Item::ArrayOfTables(tables) => Json::Array(
                tables
                    .iter()
                    .map(|nested| table_to_json(nested, &path))
                    .collect(),
            ),
            other => edit_to_json(other, is_secret_path(&path)),
        };
        map.insert(key.to_string(), value);
    }
    Json::Object(map)
}

/// Paths whose file-tree values are masked (they are secrets). Any leaf
/// named `api_key` masks — provider credentials live at
/// `providers.<id>.api_key` and inside `[[providers.custom]]` entries, not
/// just at the root.
fn is_secret_path(path: &str) -> bool {
    path == "sandbox_api_key" || path == "api_key" || path.ends_with(".api_key")
}

/// The selected preset's baseline value for a governed path, if the tier
/// expresses one (memory_level derives both memory dials, mirroring
/// `apply_to_config`).
fn definition_baseline(definition: &PresetDefinitionToml, path: &str) -> Option<Json> {
    let bool_of = |v: Option<bool>| v.map(Json::Bool);
    let str_of = |v: Option<&str>| v.map(|s| Json::String(s.to_string()));
    match path {
        "provider" => str_of(definition.provider.as_deref()),
        "reasoning_effort" => str_of(definition.reasoning_effort.as_deref()),
        "approval_policy" => str_of(definition.approval_policy.as_deref()),
        "sandbox_mode" => str_of(definition.sandbox_mode.as_deref()),
        "max_subagents" => definition.max_subagents.map(|n| Json::Number(n.into())),
        "tools.progressive_disclosure" => bool_of(definition.tools_progressive_disclosure),
        "index.enabled" => bool_of(definition.index_enabled),
        "lsp.enabled" => bool_of(definition.lsp_enabled),
        "lsp.include_warnings" => bool_of(definition.lsp_include_warnings),
        "snapshots.enabled" => bool_of(definition.snapshots_enabled),
        "context.enabled" => bool_of(definition.context_enabled),
        "context.project_pack" => bool_of(definition.context_project_pack),
        "capacity.enabled" => bool_of(definition.capacity_enabled),
        "auto.cost_saving" => bool_of(definition.auto_cost_saving),
        "network.audit" => bool_of(definition.network_audit),
        "strict_tool_mode" => bool_of(definition.strict_tool_mode),
        "memory.enabled" | "memory.kod_enabled" => {
            let derived = definition
                .memory_level
                .as_deref()
                .and_then(MemoryLevel::from_setting);
            match path {
                "memory.enabled" => definition.memory_enabled.or(match derived? {
                    MemoryLevel::Goldfish => Some(false),
                    _ => Some(true),
                }),
                _ => definition.memory_kod_enabled.or(match derived? {
                    MemoryLevel::Elephant => Some(true),
                    _ => Some(false),
                }),
            }
            .map(Json::Bool)
        }
        _ => path.strip_prefix("features.").and_then(|feature| {
            definition
                .features
                .as_ref()
                .and_then(|map| map.get(feature))
                .map(|b| Json::Bool(*b))
        }),
    }
}

/// Catalog paths the folded CLI [`Config`] does not carry — file-layer
/// keys read at load time elsewhere (`ConfigToml` / headless flags). Their
/// snapshot value comes from the file itself.
const NON_RUNTIME_KEYS: &[&str] = &["model", "output_mode", "log_level"];

/// Effective value of a catalog path in the running (folded) config.
/// `None` ⇒ key unset (`null` on the wire, `set: false`).
fn effective_value(config: &Config, path: &str) -> Option<Json> {
    let s = |v: Option<&str>| v.map(|v| Json::String(v.to_string()));
    let b = |v: Option<bool>| v.map(Json::Bool);
    let list = |v: Option<&Vec<String>>| {
        v.map(|items| Json::Array(items.iter().map(|i| Json::String(i.clone())).collect()))
    };
    match path {
        "provider" => s(config.provider.as_deref()),
        "custom_provider" => s(config.custom_provider.as_deref()),
        "turn_executor" => s(config.turn_executor.as_deref()),
        "model" => None, // NON_RUNTIME_KEYS: file-layer value
        "default_text_model" => s(config.default_text_model.as_deref()),
        "reasoning_effort" => s(config.reasoning_effort.as_deref()),
        "max_subagents" => config
            .max_subagents
            .map(|n| Json::Number(serde_json::Number::from(n as u64))),
        "auth_mode" => s(config.auth_mode.as_deref()),
        "output_mode" => None, // NON_RUNTIME_KEYS: file-layer value
        "log_level" => None,   // NON_RUNTIME_KEYS: file-layer value
        "approval_policy" => s(config.approval_policy.as_deref()),
        "sandbox_mode" => s(config.sandbox_mode.as_deref()),
        "strict_tool_mode" => b(config.strict_tool_mode),
        "api_key" => config.api_key.as_ref().map(|_| Json::Null),
        "base_url" => s(config.base_url.as_deref()),
        "http_headers" => config.http_headers.as_ref().map(|headers| {
            Json::Object(
                headers
                    .iter()
                    .map(|(k, v)| (k.clone(), Json::String(v.clone())))
                    .collect(),
            )
        }),
        "telemetry" => b(config.telemetry),
        "compaction.strategy" => s(config
            .compaction
            .as_ref()
            .and_then(|c| c.strategy.as_deref())),
        "snapshots.enabled" => b(config.snapshots.as_ref().and_then(|s| s.enabled)),
        "snapshots.max_age_days" => config
            .snapshots
            .as_ref()
            .map(|s| Json::Number(serde_json::Number::from(s.max_age_days))),
        "edit.parse_gate" => b(config.edit.as_ref().and_then(|e| e.parse_gate)),
        "skills.registry_url" => s(config
            .skills
            .as_ref()
            .and_then(|s| s.registry_url.as_deref())),
        "skills.max_install_size_bytes" => config
            .skills
            .as_ref()
            .and_then(|s| s.max_install_size_bytes)
            .map(|n| Json::Number(n.into())),
        "lsp.enabled" => b(config.lsp.as_ref().and_then(|l| l.enabled)),
        "lsp.include_warnings" => b(config.lsp.as_ref().and_then(|l| l.include_warnings)),
        "lsp.poll_after_edit_ms" => config
            .lsp
            .as_ref()
            .and_then(|l| l.poll_after_edit_ms)
            .map(|n| Json::Number(n.into())),
        "hook_sinks.unix_socket_path" => s(None),
        "desktop.enabled" => b(config.desktop.as_ref().and_then(|d| d.enabled)),
        "tools.progressive_disclosure" => {
            b(config.tools.as_ref().and_then(|t| t.progressive_disclosure))
        }
        "tools.always_load" => list(config.tools.as_ref().map(|t| &t.always_load)),
        "memory.enabled" => b(config.memory.as_ref().and_then(|m| m.enabled)),
        "memory.kod_enabled" => b(config.memory.as_ref().and_then(|m| m.kod_enabled)),
        "context.enabled" => b(config.context.enabled),
        "context.project_pack" => b(config.context.project_pack),
        "index.enabled" => b(config.index.as_ref().and_then(|i| i.enabled)),
        "capacity.enabled" => b(config.capacity.as_ref().and_then(|c| c.enabled)),
        "auto.cost_saving" => b(config.auto.as_ref().and_then(|a| a.cost_saving)),
        "network.audit" => b(config.network.as_ref().and_then(|n| n.audit)),
        "network.default" => s(config.network.as_ref().map(|n| n.default.as_str())),
        "network.allow" => list(config.network.as_ref().map(|n| &n.allow)),
        "network.deny" => list(config.network.as_ref().map(|n| &n.deny)),
        "network.proxy" => list(config.network.as_ref().map(|n| &n.proxy)),
        "sandbox.enabled" => b(config.sandbox.as_ref().and_then(|s| s.enabled)),
        "sandbox.fail_if_unavailable" => {
            b(config.sandbox.as_ref().and_then(|s| s.fail_if_unavailable))
        }
        "sandbox.enabled_platforms" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.enabled_platforms.as_ref()),
        ),
        "sandbox.excluded_commands" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.excluded_commands.as_ref()),
        ),
        "sandbox.auto_allow" => list(config.sandbox.as_ref().and_then(|s| s.auto_allow.as_ref())),
        "sandbox.auto_allow_bash_if_sandboxed" => b(config
            .sandbox
            .as_ref()
            .and_then(|s| s.auto_allow_bash_if_sandboxed)),
        "sandbox.prefer_bwrap" => b(config.sandbox.as_ref().and_then(|s| s.prefer_bwrap)),
        "sandbox.filesystem.mode" => s(config
            .sandbox
            .as_ref()
            .and_then(|s| s.filesystem.as_ref())
            .and_then(|f| f.mode.as_deref())),
        "sandbox.filesystem.writable_roots" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.filesystem.as_ref())
                .and_then(|f| f.writable_roots.as_ref()),
        ),
        "sandbox.filesystem.allow_read" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.filesystem.as_ref())
                .and_then(|f| f.allow_read.as_ref()),
        ),
        "sandbox.filesystem.deny_read" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.filesystem.as_ref())
                .and_then(|f| f.deny_read.as_ref()),
        ),
        "sandbox.filesystem.allow_write" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.filesystem.as_ref())
                .and_then(|f| f.allow_write.as_ref()),
        ),
        "sandbox.filesystem.deny_write" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.filesystem.as_ref())
                .and_then(|f| f.deny_write.as_ref()),
        ),
        "sandbox.filesystem.exclude_tmpdir" => b(config
            .sandbox
            .as_ref()
            .and_then(|s| s.filesystem.as_ref())
            .and_then(|f| f.exclude_tmpdir)),
        "sandbox.filesystem.exclude_slash_tmp" => b(config
            .sandbox
            .as_ref()
            .and_then(|s| s.filesystem.as_ref())
            .and_then(|f| f.exclude_slash_tmp)),
        "sandbox.network.enabled" => b(config
            .sandbox
            .as_ref()
            .and_then(|s| s.network.as_ref())
            .and_then(|n| n.enabled)),
        "sandbox.network.allow_managed_domains_only" => b(config
            .sandbox
            .as_ref()
            .and_then(|s| s.network.as_ref())
            .and_then(|n| n.allow_managed_domains_only)),
        "sandbox.network.allow" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.network.as_ref())
                .and_then(|n| n.allow.as_ref()),
        ),
        "sandbox.network.deny" => list(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.network.as_ref())
                .and_then(|n| n.deny.as_ref()),
        ),
        "sandbox_backend" => s(config.sandbox_backend.as_deref()),
        "sandbox_url" => s(config.sandbox_url.as_deref()),
        "sandbox_api_key" => config.sandbox_api_key.as_ref().map(|_| Json::Null),
        "preset" => s(config.preset.as_deref()),
        "rhai_loop" | "providers" => None,
        _ => path.strip_prefix("features.").and_then(|feature| {
            b(config
                .features
                .as_ref()
                .and_then(|f| f.entries.get(feature))
                .copied())
        }),
    }
}

fn selection_source(config: &Config) -> &'static str {
    if config.preset.is_some() || config.mode.is_some() {
        "config"
    } else if crate::presets::persisted_selection().is_some() {
        "settings"
    } else {
        "default"
    }
}

/// Build the full snapshot: tiers, selection, per-key state, file tree.
pub fn snapshot(
    startup: &Config,
    workspace: &Path,
    config_path: &Path,
) -> anyhow::Result<PresetSnapshot> {
    snapshot_with(startup, config_path, &PresetCatalog::load(Some(workspace)))
}

/// Testable core: [`snapshot`] against a caller-built catalog.
fn snapshot_with(
    startup: &Config,
    config_path: &Path,
    preset_catalog: &PresetCatalog,
) -> anyhow::Result<PresetSnapshot> {
    let raw = read_config_or_empty(config_path)
        .map_err(|err| anyhow::anyhow!("read config.toml: {err}"))?;
    let doc: DocumentMut = raw
        .parse()
        .map_err(|err| anyhow::anyhow!("config.toml parse error: {err}"))?;
    // Selection prefers the *file* layer: after `selectPreset` the snapshot
    // must show what the next launch folds, not the stale running choice.
    let file_selection = doc_lookup_json(&doc, "preset")
        .and_then(|value| value.as_str().map(str::to_string))
        .or_else(|| {
            doc_lookup_json(&doc, "mode").and_then(|value| value.as_str().map(str::to_string))
        });
    let selection_raw = file_selection
        .clone()
        .or_else(|| startup.preset_selection().map(str::to_string))
        .unwrap_or_else(|| "middle".to_string());
    let selection = canonicalize_preset_name(&selection_raw)
        .map(|(name, _)| name)
        .unwrap_or_else(|_| "middle".to_string());
    let loaded = preset_catalog
        .get_ci(&selection)
        .cloned()
        .unwrap_or_else(|| {
            preset_catalog
                .get_ci("middle")
                .cloned()
                .expect("middle is compiled in")
        });

    // Present the launch-equivalent view: `codesmith serve` does not fold
    // the preset layer at startup (the REPL path does), so fold the
    // resolved tier into a clone here — the same rationale as `codesmith
    // doctor` folding "so the reported tier (and the effective switches)
    // match an interactive session". Idempotent for an already-folded
    // config: fill-if-unset finds the filled values and no-ops.
    let mut view = startup.clone();
    view.preset_deviated |= crate::presets::apply_to_config(&mut view, &loaded.definition);
    if view.preset.is_none() {
        view.preset = Some(loaded.name.clone());
    }

    let tiers: Vec<TierView> = preset_catalog
        .iter()
        .map(|loaded| TierView {
            name: loaded.name.clone(),
            description: loaded.definition.description.clone().unwrap_or_default(),
            source: loaded.source.label().to_string(),
        })
        .collect();

    let mut keys = Vec::new();
    let mut deviated = Vec::new();
    // The presented view folds the file-selected tier; the running process
    // keeps its startup selection until the next launch. That divergence is
    // restart-pending exactly like a dial whose file value differs from the
    // running fold — the fold above makes `view.preset` equal the file
    // selection, so the per-key comparison below cannot see it on its own.
    let startup_tier = canonicalize_preset_name(startup.preset_selection().unwrap_or("middle"))
        .map(|(name, _)| name)
        .unwrap_or_else(|_| "middle".to_string());
    let mut restart_pending = selection != startup_tier;
    for entry in catalog() {
        // File value stays unmasked for comparisons (source, deviation,
        // restart); display masking happens per-path in `file_tree` and via
        // the masked `value` below.
        let file_value = doc_lookup_json(&doc, entry.path);
        let file_raw_present = file_value.is_some();
        let baseline = definition_baseline(&loaded.definition, entry.path);
        // File-layer keys (model / output_mode / log_level) are not carried
        // by the folded Config — the file value *is* the effective one.
        let effective = if NON_RUNTIME_KEYS.contains(&entry.path) {
            file_value.clone()
        } else {
            effective_value(&view, entry.path)
        };
        let set = effective.is_some();
        let mut value = effective.unwrap_or(Json::Null);
        let source = if file_raw_present {
            KeySource::ExplicitFile
        } else if entry.preset_governed
            && baseline.is_some()
            && value == baseline.clone().unwrap_or(Json::Null)
        {
            KeySource::Baseline
        } else if set {
            KeySource::ExplicitOther
        } else {
            KeySource::Default
        };
        if entry.preset_governed && file_raw_present && baseline.is_some() && file_value != baseline
        {
            deviated.push(entry.path.to_string());
        }
        // Secrets compare as "always pending" against their masked value —
        // skip them; a secret change needs a restart either way and the
        // notice is already up after any edit batch. Whole-table rows are
        // read-only passthroughs: their presence in the file is not a
        // pending change.
        if file_raw_present
            && file_value.as_ref() != Some(&value)
            && !matches!(entry.kind, KeyKind::Secret | KeyKind::Table)
        {
            restart_pending = true;
        }
        if matches!(entry.kind, KeyKind::Secret) {
            value = Json::Null;
        }
        let baselines = if entry.preset_governed {
            preset_catalog
                .iter()
                .filter_map(|tier| {
                    definition_baseline(&tier.definition, entry.path).map(|value| BaselineView {
                        tier: tier.name.clone(),
                        value,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let (kind, options) = match entry.kind {
            KeyKind::Enum { options } => ("enum", Some(options)),
            KeyKind::Bool => ("bool", None),
            KeyKind::String => ("string", None),
            KeyKind::Secret => ("secret", None),
            KeyKind::Int => ("int", None),
            KeyKind::StringList => ("string-list", None),
            KeyKind::StringMap => ("string-map", None),
            KeyKind::Table => ("table", None),
        };
        keys.push(KeyStateView {
            path: entry.path.to_string(),
            group: entry.group,
            preset_governed: entry.preset_governed,
            read_only: entry.read_only,
            secret: matches!(entry.kind, KeyKind::Secret),
            kind,
            options,
            label_en: entry.label_en.to_string(),
            label_zh: entry.label_zh.to_string(),
            desc_en: entry.desc_en.to_string(),
            desc_zh: entry.desc_zh.to_string(),
            value,
            set,
            source,
            baselines,
            resettable: matches!(source, KeySource::ExplicitFile)
                && !entry.read_only
                && !matches!(entry.kind, KeyKind::Table),
        });
    }

    let effective = if view.preset_deviated || !deviated.is_empty() {
        "diy".to_string()
    } else {
        loaded.name.clone()
    };

    Ok(PresetSnapshot {
        tiers,
        selection: SelectionView {
            selected: loaded.name.clone(),
            selected_source: if file_selection.is_some() {
                "config".to_string()
            } else {
                selection_source(startup).to_string()
            },
            effective,
            deviated,
        },
        keys,
        file_tree: table_to_json(doc.as_table(), ""),
        restart_pending,
        warnings: preset_catalog.warnings.clone(),
    })
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

fn json_to_toml(value: &Json) -> Option<toml::Value> {
    match value {
        Json::Null => None,
        Json::Bool(b) => Some(toml::Value::Boolean(*b)),
        Json::Number(n) => n
            .as_i64()
            .map(toml::Value::Integer)
            .or_else(|| n.as_f64().map(toml::Value::Float)),
        Json::String(s) => Some(toml::Value::String(s.clone())),
        Json::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_toml(item)?);
            }
            Some(toml::Value::Array(out))
        }
        Json::Object(map) => {
            let mut table = toml::Table::new();
            for (key, val) in map {
                table.insert(key.clone(), json_to_toml(val)?);
            }
            Some(toml::Value::Table(table))
        }
    }
}

/// Type-check a JSON value against a catalog kind; `Err` carries the
/// human-readable reason.
fn check_kind(kind: KeyKind, value: &Json) -> Result<(), String> {
    let what = match (kind, value) {
        (KeyKind::Enum { options }, Json::String(s)) => {
            if options.contains(&s.as_str()) {
                return Ok(());
            }
            format!("value {s:?} not in [{}]", options.join(", "))
        }
        (KeyKind::Enum { .. }, _) => "expected a string".to_string(),
        (KeyKind::Bool, Json::Bool(_)) => return Ok(()),
        (KeyKind::Bool, _) => "expected a boolean".to_string(),
        (KeyKind::String, Json::String(_)) | (KeyKind::Secret, Json::String(_)) => return Ok(()),
        (KeyKind::String, _) | (KeyKind::Secret, _) => "expected a string".to_string(),
        (KeyKind::Int, Json::Number(n)) if n.is_i64() => return Ok(()),
        (KeyKind::Int, _) => "expected an integer".to_string(),
        (KeyKind::StringList, Json::Array(items)) => {
            if items.iter().all(|item| matches!(item, Json::String(_))) {
                return Ok(());
            }
            "expected an array of strings".to_string()
        }
        (KeyKind::StringList, _) => "expected an array of strings".to_string(),
        (KeyKind::StringMap, Json::Object(map)) => {
            if map.values().all(|v| matches!(v, Json::String(_))) {
                return Ok(());
            }
            "expected an object of strings".to_string()
        }
        (KeyKind::StringMap, _) => "expected an object of strings".to_string(),
        (KeyKind::Table, _) => return Err("table keys are read-only".to_string()),
    };
    Err(what)
}

/// Apply a batch of ops to the user's config.toml and return the fresh
/// snapshot (computed against the *running* fold, so `restartPending`
/// flips on the first edit).
pub fn apply(
    ops: &[PresetOp],
    startup: &Config,
    workspace: &Path,
    config_path: &Path,
) -> Result<PresetSnapshot, ApplyError> {
    let raw = read_config_or_empty(config_path)
        .map_err(|err| ApplyError::new("io", format!("read config.toml: {err}")))?;
    let mut doc: DocumentMut = raw.parse().map_err(|err| {
        ApplyError::new("invalid_file", format!("config.toml parse error: {err}"))
    })?;

    let preset_catalog = PresetCatalog::load(Some(workspace));
    for op in ops {
        match op {
            PresetOp::SelectPreset { name } => {
                let canonical = canonicalize_preset_name(name)
                    .map_err(|err| ApplyError::new("bad_preset", format!("{err}")))?
                    .0;
                if preset_catalog.get_ci(&canonical).is_none() {
                    return Err(ApplyError::new(
                        "bad_preset",
                        format!(
                            "unknown preset {canonical:?}; available: {:?}",
                            preset_catalog.names()
                        ),
                    ));
                }
                doc_set(&mut doc, "preset", toml::Value::String(canonical))
                    .map_err(|err| ApplyError::new("bad_value", err))?;
                // The deprecated alias would keep outranking nothing but
                // dirties the file; consume it like the startup fold does.
                doc_unset(&mut doc, "mode").map_err(|err| ApplyError::new("bad_value", err))?;
            }
            PresetOp::Set { path, value } => {
                if value.is_null() {
                    // `set: null` reads as unset — keeps the UI single-op.
                    refuse_table_target(&doc, path)?;
                    doc_unset(&mut doc, path).map_err(|err| ApplyError::new("bad_value", err))?;
                    continue;
                }
                let entry = catalog().iter().find(|entry| entry.path == path.as_str());
                match entry {
                    Some(entry) if entry.read_only => {
                        return Err(ApplyError::new(
                            "read_only",
                            format!("{path:?} is read-only (owned by a dedicated editor)"),
                        ));
                    }
                    Some(entry) => {
                        check_kind(entry.kind, value).map_err(|reason| {
                            ApplyError::new("bad_value", format!("{path}: {reason}"))
                        })?;
                    }
                    None => {
                        // Not curated: only leaf values already present in
                        // the file may be edited (no conjuring new keys from
                        // the UI, no overwriting whole sections either).
                        if !doc_has(&doc, path) {
                            return Err(ApplyError::new(
                                "unknown_key",
                                format!(
                                    "{path:?} is not a known config key and is not present in config.toml"
                                ),
                            ));
                        }
                        refuse_table_target(&doc, path)?;
                    }
                }
                let toml_value = json_to_toml(value).ok_or_else(|| {
                    ApplyError::new("bad_value", format!("{path}: unsupported value"))
                })?;
                doc_set(&mut doc, path, toml_value)
                    .map_err(|err| ApplyError::new("bad_value", err))?;
            }
            PresetOp::Unset { path } => {
                if !doc_has(&doc, path) {
                    return Err(ApplyError::new(
                        "not_resettable",
                        format!("{path:?} is not set in config.toml"),
                    ));
                }
                // Whole tables — cataloged (`rhai_loop`, `providers`) or not
                // — are never unset: removing one drops every key under it.
                refuse_table_target(&doc, path)?;
                doc_unset(&mut doc, path).map_err(|err| ApplyError::new("bad_value", err))?;
            }
        }
    }

    // Whole-document validation: the file is never persisted in a state the
    // loader would reject.
    let rendered = doc.to_string();
    toml::from_str::<Config>(&rendered).map_err(|err| {
        ApplyError::new(
            "invalid_file",
            format!("edit would produce an invalid config.toml: {err}"),
        )
    })?;

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| ApplyError::new("io", format!("create config dir: {err}")))?;
    }
    write_config_file_private(config_path, &rendered)
        .map_err(|err| ApplyError::new("io", format!("write config.toml: {err}")))?;

    snapshot(startup, workspace, config_path)
        .map_err(|err| ApplyError::new("io", format!("re-snapshot: {err}")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::apply_to_config;

    /// Built-ins only — tests must not read the developer's ~/.codesmith.
    fn builtin_catalog() -> PresetCatalog {
        PresetCatalog::load_from(None, None)
    }

    fn folded_startup() -> Config {
        let middle = builtin_catalog()
            .get_ci("middle")
            .cloned()
            .expect("middle builtin");
        let mut config = Config::default();
        apply_to_config(&mut config, &middle.definition);
        config.preset = Some(middle.name.clone());
        config
    }

    fn temp_config(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).expect("write fixture");
        (dir, path)
    }

    fn key_state<'a>(snap: &'a PresetSnapshot, path: &str) -> &'a KeyStateView {
        snap.keys
            .iter()
            .find(|key| key.path == path)
            .unwrap_or_else(|| panic!("catalog row {path:?} missing"))
    }

    #[test]
    fn governed_keys_match_apply_to_config() {
        // The catalog's governed set must equal what the preset layer folds
        // (presets.rs): 18 scalar/section dials + the known feature keys.
        let mut governed: Vec<String> = catalog()
            .iter()
            .filter(|entry| entry.preset_governed)
            .map(|entry| entry.path.to_string())
            .collect();
        let mut expected: Vec<String> = [
            "provider",
            "reasoning_effort",
            "approval_policy",
            "sandbox_mode",
            "max_subagents",
            "tools.progressive_disclosure",
            "index.enabled",
            "lsp.enabled",
            "lsp.include_warnings",
            "snapshots.enabled",
            "memory.enabled",
            "memory.kod_enabled",
            "context.enabled",
            "context.project_pack",
            "capacity.enabled",
            "auto.cost_saving",
            "network.audit",
            "strict_tool_mode",
        ]
        .iter()
        .map(|path| path.to_string())
        .chain(FEATURE_KEYS.iter().map(|k| format!("features.{k}")))
        .collect();
        governed.sort_unstable();
        expected.sort_unstable();
        assert_eq!(governed, expected);
    }

    #[test]
    fn catalog_accessors_spot_check() {
        // The accessor match is compile-time exhaustive; spot-check that a
        // folded config surfaces values for representative keys per group.
        let startup = folded_startup();
        assert_eq!(
            effective_value(&startup, "index.enabled"),
            Some(Json::Bool(true))
        );
        assert_eq!(
            effective_value(&startup, "tools.progressive_disclosure"),
            Some(Json::Bool(true))
        );
        assert_eq!(
            effective_value(&startup, "max_subagents"),
            Some(Json::Number(10u64.into()))
        );
        // A key the tier leaves unset resolves to None (default), not panic.
        assert_eq!(effective_value(&startup, "api_key"), None);
    }

    #[test]
    fn serve_style_unfolded_config_presents_launch_equivalent_view() {
        // `codesmith serve` (the desktop backend) never folds the preset
        // layer; the snapshot must still present what a launch folds.
        let (_dir, path) = temp_config("");
        let startup = Config::default();
        let snap = snapshot_with(&startup, &path, &builtin_catalog()).expect("snapshot");
        let index = key_state(&snap, "index.enabled");
        assert_eq!(index.value, Json::Bool(true));
        assert_eq!(index.source, KeySource::Baseline);
        assert_eq!(snap.selection.effective, "middle");
        assert!(!snap.restart_pending, "the startup tier is what runs");
    }

    #[test]
    fn tier_switch_from_running_selection_is_restart_pending() {
        // The presented view folds the file-selected tier, but the running
        // serve process keeps its startup selection until the next launch —
        // a differing tier is restart-pending like any deviating dial,
        // re-selecting the startup tier is not.
        let (_dir, path) = temp_config("preset = \"all\"\n");
        let snap = snapshot_with(&Config::default(), &path, &builtin_catalog()).expect("snapshot");
        assert!(snap.restart_pending, "all selected while running middle");
        std::fs::write(&path, "preset = \"middle\"\n").expect("rewrite fixture");
        let snap = snapshot_with(&Config::default(), &path, &builtin_catalog()).expect("snapshot");
        assert!(!snap.restart_pending, "the startup tier re-selected");
    }

    #[test]
    fn source_resolution_and_deviation() {
        let (_dir, path) =
            temp_config("# my comment\nstrict_tool_mode = true\n\n[mystery]\nkeep = \"me\"\n");
        let startup = folded_startup();
        let snap = snapshot_with(&startup, &path, &builtin_catalog()).expect("snapshot");

        // File-present governed key that disagrees with middle's baseline:
        // explicit-file, resettable, and reports diy.
        let strict = key_state(&snap, "strict_tool_mode");
        assert_eq!(strict.source, KeySource::ExplicitFile);
        assert!(strict.resettable);
        assert!(
            snap.selection
                .deviated
                .contains(&"strict_tool_mode".to_string())
        );
        assert_eq!(snap.selection.effective, "diy");

        // Unset governed key that the tier filled: baseline, not resettable.
        let index = key_state(&snap, "index.enabled");
        assert_eq!(index.source, KeySource::Baseline);
        assert!(!index.resettable);
        assert_eq!(index.value, Json::Bool(true));

        // restartPending: file true vs running fold false.
        assert!(snap.restart_pending);

        // Unknown keys stay visible in the file tree.
        let tree = snap.file_tree.get("mystery").and_then(|v| v.get("keep"));
        assert_eq!(tree, Some(&Json::String("me".to_string())));
    }

    #[test]
    fn explicit_other_from_env_like_override() {
        // No file entry, but the running fold has a value the tier did not
        // provide (env / CLI flag shaped): explicit-other, not resettable,
        // and the effective tier reports diy via preset_deviated.
        let (_dir, path) = temp_config("");
        let middle = builtin_catalog()
            .get_ci("middle")
            .cloned()
            .expect("middle builtin");
        let mut startup = Config {
            max_subagents: Some(42),
            ..Config::default()
        };
        // `fold()` (apply_config_preset) is what ORs preset_deviated; emulate
        // it here since the test folds the definition directly.
        startup.preset_deviated |= apply_to_config(&mut startup, &middle.definition);
        startup.preset = Some(middle.name.clone());
        let snap = snapshot_with(&startup, &path, &builtin_catalog()).expect("snapshot");
        let dial = key_state(&snap, "max_subagents");
        assert_eq!(dial.source, KeySource::ExplicitOther);
        assert!(!dial.resettable);
        assert_eq!(snap.selection.effective, "diy");
    }

    #[test]
    fn secrets_are_masked_everywhere() {
        let (_dir, path) = temp_config("api_key = \"sk-secret\"\n");
        let mut startup = folded_startup();
        startup.api_key = Some("sk-secret".to_string());
        let snap = snapshot_with(&startup, &path, &builtin_catalog()).expect("snapshot");
        let key = key_state(&snap, "api_key");
        assert!(key.secret);
        assert_eq!(key.value, Json::Null);
        assert!(key.set);
        assert_eq!(
            snap.file_tree.get("api_key"),
            Some(&Json::String("•••".to_string()))
        );
    }

    #[test]
    fn provider_api_keys_are_masked_in_the_file_tree() {
        // Credentials live at `providers.<id>.api_key` and inside
        // `[[providers.custom]]` entries — every leaf named `api_key` must
        // mask in the served file tree, not just the root slot.
        let (_dir, path) = temp_config(
            "[providers.openai]\napi_key = \"sk-live\"\nbase_url = \"https://x\"\n\n\
             [[providers.custom]]\nid = \"mine\"\napi_key = \"sk-custom\"\n",
        );
        let startup = folded_startup();
        let snap = snapshot_with(&startup, &path, &builtin_catalog()).expect("snapshot");
        assert_eq!(
            snap.file_tree.pointer("/providers/openai/api_key"),
            Some(&Json::String("•••".to_string()))
        );
        assert_eq!(
            snap.file_tree.pointer("/providers/custom/0/api_key"),
            Some(&Json::String("•••".to_string()))
        );
        // Non-secret neighbors stay readable.
        assert_eq!(
            snap.file_tree.pointer("/providers/openai/base_url"),
            Some(&Json::String("https://x".to_string()))
        );
    }

    #[test]
    fn unreadable_config_fails_loud_not_as_empty() {
        // A directory at the config path reads as an IO error, not NotFound:
        // snapshot must fail rather than present a phantom empty file, and
        // apply must refuse rather than write ops applied to that phantom
        // back over the real path.
        let dir = tempfile::tempdir().expect("tempdir");
        let bogus = dir.path().join("config.toml");
        std::fs::create_dir_all(&bogus).expect("make dir at config path");
        let startup = folded_startup();
        assert!(snapshot_with(&startup, &bogus, &builtin_catalog()).is_err());
        let err = apply(
            &[PresetOp::Set {
                path: "model".to_string(),
                value: Json::String("x".to_string()),
            }],
            &startup,
            Path::new("/nonexistent-workspace"),
            &bogus,
        )
        .unwrap_err();
        assert_eq!(err.code, "io");
    }

    #[test]
    fn whole_table_targets_are_refused_not_destroyed() {
        // Set/unset on a table path — cataloged or not, null-set included —
        // must refuse rather than overwrite/drop every key under the
        // section.
        let (_dir, path) = temp_config("[network]\nallow = [\"a\"]\ndeny = [\"b\"]\n");
        let startup = folded_startup();
        let ws = Path::new("/nonexistent-workspace");
        let before = std::fs::read_to_string(&path).expect("read");

        let set_table = apply(
            &[PresetOp::Set {
                path: "network".to_string(),
                value: Json::Bool(true),
            }],
            &startup,
            ws,
            &path,
        )
        .unwrap_err();
        assert_eq!(set_table.code, "read_only");

        let null_set_table = apply(
            &[PresetOp::Set {
                path: "network".to_string(),
                value: Json::Null,
            }],
            &startup,
            ws,
            &path,
        )
        .unwrap_err();
        assert_eq!(null_set_table.code, "read_only");

        let unset_table = apply(
            &[PresetOp::Unset {
                path: "network".to_string(),
            }],
            &startup,
            ws,
            &path,
        )
        .unwrap_err();
        assert_eq!(unset_table.code, "read_only");

        // Cataloged table unset keeps refusing through the same shape check.
        let (_d2, p2) = temp_config("[rhai_loop]\nheartbeat_s = 1\n");
        let err = apply(
            &[PresetOp::Unset {
                path: "rhai_loop".to_string(),
            }],
            &startup,
            ws,
            &p2,
        )
        .unwrap_err();
        assert_eq!(err.code, "read_only");

        // An array-of-tables target is a whole table too: this is the one
        // shape the old leaf-only walk did see, so an unset here really
        // would have dropped every [[providers.custom]] entry.
        let (_d3, p3) = temp_config(
            "[providers.openai]\napi_key = \"sk\"\n\n[[providers.custom]]\nid = \"mine\"\n",
        );
        let err = apply(
            &[PresetOp::Unset {
                path: "providers.custom".to_string(),
            }],
            &startup,
            ws,
            &p3,
        )
        .unwrap_err();
        assert_eq!(err.code, "read_only");
        let after = std::fs::read_to_string(&p3).expect("read back");
        assert!(after.contains("[[providers.custom]]"), "{after}");

        // Rejected batches leave the file untouched.
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
    }

    #[test]
    fn apply_set_updates_file_and_preserves_comments() {
        let (_dir, path) = temp_config("# keep\nmodel = \"deepseek-chat\"\n");
        let startup = folded_startup();
        let snap = apply(
            &[PresetOp::Set {
                path: "memory.enabled".to_string(),
                value: Json::Bool(false),
            }],
            &startup,
            Path::new("/nonexistent-workspace"),
            &path,
        )
        .expect("apply");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("# keep"));
        assert!(text.contains("[memory]"));
        assert!(text.contains("enabled = false"));
        // The *running* fold still has the old value — that is exactly what
        // restartPending reports; the file tree carries the new one.
        assert_eq!(key_state(&snap, "memory.enabled").value, Json::Bool(true));
        assert_eq!(
            snap.file_tree.pointer("/memory/enabled"),
            Some(&Json::Bool(false))
        );
        assert!(snap.restart_pending);
    }

    #[test]
    fn apply_rejects_bad_ops() {
        let (_dir, path) = temp_config("model = \"deepseek-chat\"\n");
        let startup = folded_startup();
        let ws = Path::new("/nonexistent-workspace");

        let unknown = apply(
            &[PresetOp::Set {
                path: "totally.made_up".to_string(),
                value: Json::Bool(true),
            }],
            &startup,
            ws,
            &path,
        );
        assert_eq!(unknown.unwrap_err().code, "unknown_key");

        let bad_enum = apply(
            &[PresetOp::Set {
                path: "reasoning_effort".to_string(),
                value: Json::String("ultra".to_string()),
            }],
            &startup,
            ws,
            &path,
        );
        assert_eq!(bad_enum.unwrap_err().code, "bad_value");

        let diy = apply(
            &[PresetOp::SelectPreset {
                name: "diy".to_string(),
            }],
            &startup,
            ws,
            &path,
        );
        assert_eq!(diy.unwrap_err().code, "bad_preset");

        let unset_absent = apply(
            &[PresetOp::Unset {
                path: "strict_tool_mode".to_string(),
            }],
            &startup,
            ws,
            &path,
        );
        assert_eq!(unset_absent.unwrap_err().code, "not_resettable");

        // File unchanged after rejected batches.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "model = \"deepseek-chat\"\n"
        );
    }

    #[test]
    fn apply_select_preset_writes_key_and_consumes_alias() {
        let (_dir, path) = temp_config("mode = \"simple\"\n");
        let startup = folded_startup();
        let snap = apply(
            &[PresetOp::SelectPreset {
                name: "all".to_string(),
            }],
            &startup,
            Path::new("/nonexistent-workspace"),
            &path,
        )
        .expect("apply");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("preset = \"all\""));
        assert!(!text.contains("mode ="));
        assert_eq!(snap.selection.selected, "all");
    }

    #[test]
    fn apply_set_null_unsets_and_generic_edit_needs_presence() {
        let (_dir, path) = temp_config("[mystery]\nkeep = 1\n");
        let startup = folded_startup();
        let ws = Path::new("/nonexistent-workspace");

        // set null == unset for a file-present key.
        let snap = apply(
            &[PresetOp::Set {
                path: "mystery.keep".to_string(),
                value: Json::Null,
            }],
            &startup,
            ws,
            &path,
        )
        .expect("apply null-unset");
        assert!(
            snap.file_tree.get("mystery").is_none()
                || snap.file_tree["mystery"].get("keep").is_none()
        );

        // A file-present non-catalog key may be edited generically…
        let (_d2, p2) = temp_config("[mystery]\nkeep = 1\n");
        apply(
            &[PresetOp::Set {
                path: "mystery.keep".to_string(),
                value: Json::Number(2.into()),
            }],
            &startup,
            ws,
            &p2,
        )
        .expect("generic edit of present key");

        // …but a wholly new non-catalog key cannot be conjured.
        let err = apply(
            &[PresetOp::Set {
                path: "mystery.appeared".to_string(),
                value: Json::Bool(true),
            }],
            &startup,
            ws,
            &p2,
        )
        .unwrap_err();
        assert_eq!(err.code, "unknown_key");
    }
}
