# CodeSmith 扩展系统 — 设计规格

- **日期:** 2026-07-21
- **状态:** 设计(等待 `writing-plans` skill 出实施计划)
- **范围:** 与 pi-mono 的 extension 模型全量平价,**分阶段**(多 slice,ROADMAP §F)
- **分支:** `feat/pluggable-framework-core`(本设计开一个新的 ROADMAP §F section)
- **前置:** §E "Framework core growth (LangChain parity)"——provider seam
  (`LlmClient` / `ProviderFactory` / `ProviderRegistry`)与框架核心 traits
  (`Tool` / `ChatHistory` / `Callback` / `AgentExecutor`)已落地。本设计在其上构建 §E 未覆盖的 **extension system** 层。
- **参考:** `/Users/camile/Work/TypeScript/pi-mono`——被移植的 extension 模型(概念 = "extension";`ExtensionFactory` / `ExtensionAPI` / `ExtensionContext` / ~30 个生命周期事件 / `EventBus` / 发现 + manifest / stale-context guard)。

---

## 1. 背景与动机

### 1.1 pi-mono 有什么(我们要移植的模型)

pi-mono 的可扩展性原语是 **extension**:一个 TypeScript 模块,导出默认 factory `ExtensionFactory = (pi: ExtensionAPI) => void | Promise<void>`。factory 接收一个 `ExtensionAPI`,以**命令式**(非声明式 manifest)注册贡献:

- `pi.registerTool(ToolDefinition)`——LLM 可调用的 tool(TypeBox 参数 schema)
- `pi.registerCommand(name, opts)`——slash command
- `pi.registerShortcut(key, opts)`——键盘快捷键(保留键位列表)
- `pi.registerFlag(name, opts)`——CLI flag(first-wins)
- `pi.registerMessageRenderer(customType, fn)` / `pi.registerEntryRenderer(...)`——自定义 transcript/entry renderer(每个 `customType` first-wins)
- `pi.registerProvider(...)` / `pi.unregisterProvider(name)`——运行时 LLM provider/model 注册(在 host `bindCore` 时 flush 进 `ModelRegistry`)
- `pi.on(event, handler)`——~30 个生命周期事件
  (`ProjectTrust` / `SessionStart` / `ResourcesDiscover` / `Input` /
  `BeforeAgentStart` / `TurnStart` / `BeforeProviderHeaders` /
  `BeforeProviderRequest` / `AfterProviderResponse` / `ToolExecutionStart` /
  `ToolCall`(block)/ `ToolExecutionUpdate` / `ToolResult`(modify)/
  `ToolExecutionEnd` / `TurnEnd` / `AgentEnd` / `AgentSettled` /
  `SessionBeforeSwitch` / `SessionBeforeFork` / `SessionShutdown` /
  `SessionBeforeCompact` / `SessionCompact` / …)。Handler 的返回类型携带 cancel / transform / block 语义,跨 handler 链式传递。
- `pi.events`——扩展间 `EventBus`(命名空间 pub/sub channel)
- Action 表面:`sendMessage` / `sendUserMessage` / `appendEntry` /
  `setSessionName` / `setLabel` / `exec` / `setModel` / `getActiveTools` /
  `setActiveTools` / `getCommands` / `setThinkingLevel` / `compact` /
  `getContextUsage` / `getSystemPrompt` / …

**生命周期:** 启动时 eager load(jiti 动态 TS 源码 import);两阶段构造——loader 构建一个 `ExtensionRuntime`,其 action 方法是**抛错的 stub**,并把 provider 注册排队进 `pending_*`;host 的 `ExtensionRunner::bindCore` 随后换入真正的 action 实现,并把 `pending_*` flush 进 host 注册点。从三个源发现(项目本地 `.pi/extensions/`、全局 `~/.pi/agent/extensions/`、显式配置路径),一层深,约定目录 + `package.json` 的 `pi` manifest。项目本地 extension 仅在 `project_trust` 事件后加载。

**冲突解决:** tool/flag/renderer first-wins;command 用 `:N` 后缀(`name`、`name:2`、`name:3`);shortcut 用保留键位列表;冲突以 `ResourceDiagnostic` 上报,**不崩溃**——所有 extension 保持加载。

**Service locator / DI:** 一个由 loader 拥有、由 runner 填充的共享可变 `ExtensionRuntime` action 包;每个 `ExtensionAPI` 引用同一个 runtime,所以 host 能在 `bindCore` 换实现,所有 extension 懒看到新行为。

**Stale-context guard:** runtime 上的 generation 计数器;`invalidate()` 在 `newSession` / `fork` / `switchSession` / `reload` 后使旧 runtime 捕获的 `pi` / `ctx` 抛 stale-context 错。

**沙箱立场:** **by design 明确不做**——"extension 以你的完整系统权限运行,可执行任意代码;只从你信任的源安装。"项目本地 extension 仅在 `project_trust` 后加载。隔离委托给容器化(Gondolin micro-VM、plain Docker)。stale-context guard 是 runtime 内唯一的安全边界;错误隔离(per-handler try/catch)防止单个 extension 崩溃其他。

**嵌入:** `createAgentSession(options)`——嵌入者传 `customTools`、`resourceLoader`(拼入 inline `InlineExtension` factory)、UI context 等。

### 1.2 CodeSmith 有什么(目标)与缺口

CodeSmith 是 9-crate Rust workspace。§E "pluggable framework core" 工作(slices 1–53)已**吸收了 pi-mono 的 provider seam 与框架核心 traits**(LangChain 平价:`BaseTool` / `Memory` / `Callbacks` / `AgentExecutor` 等价物):

- `codesmith-agent`(core):`LlmClient`、`ProviderFactory`、`ProviderRegistry`、`ProviderId`(开放 union:`Builtin(ProviderKind)` | `Custom(String)`)、`ProviderConfig`(中性构造输入)、`Tool`、`ChatHistory`、`Callback`、`AgentExecutor`、`DefaultAgentExecutor`。
- `codesmith-agent-runtime`:`ToolSpec` trait(生产 tool 契约,带 capability 驱动的 `ApprovalRequirement`)、`ToolSpecAdapter`(core `Tool` → `ToolSpec`)、`CallbackBridge`(`Callback` → host `Event` + `HookHost`)、`SessionChatHistory`(`Session` → `ChatHistory`)、`HostAgentExecutor`(活的生产 agent loop,带 probe 式 `Option<…>` 协作者:LSP / steer / approval / compaction / capacity / subagent——一个潜在的 service-locator 形状)。

CodeSmith **现有的扩展点碎片化**:

| 关注点 | 当前形态 | open/closed/config-driven? |
|---|---|---|
| Tools | `ToolSpec` trait + `ToolRegistry` + plugin-tools dir `~/.codesmith/tools/` | **open**(trait 对象 + dir) |
| LLM providers | rig factory + `[[providers.custom]]` config | config-driven(**无运行时注册**) |
| MCP servers | `McpPool`,config-file-driven,mtime+hash 懒自动 reload | config-driven |
| Slash commands | `COMMANDS` 静态数组 + `execute()` 里 `match` 臂 | **closed**(必须改源码)——但 user-defined command 可覆盖 built-in |
| Skills | `SkillRegistry`(fs scan)+ `SkillStateStore`(TOML enable/disable) | config-driven + stateful |

**缺口:** CodeSmith 有零件,但它们孤立、大多 closed(commands)或 config-driven(providers / MCP)。pi-mono 把**一切统一在一个 extension 概念下**:命令式注册 + 丰富事件生命周期 + 扩展间 bus + 运行时 provider 注册 + 统一发现/manifest + stale-context guard。这些在 CodeSmith 都不存在。`grep ROADMAP.md` 确认 "extension system" / "plugin system" / "lifecycle event" / "event bus" / "ExtensionAPI" 0 命中——这是**新工作**,不是对现有 ROADMAP 条目的细化。

### 1.3 目标

在 §E 框架核心之上构建 pi-mono 式的 **extension system**,把碎片化的扩展点统一在一个 `Extension` 概念下,带命令式注册、生命周期事件系统、扩展间 bus、运行时 provider 注册、统一发现/manifest、stale-context guard——**分阶段**跨多 slice 交付,slice 1 落地完整可测的架构,长尾逐 slice 安装。

---

## 2. 关键决策(四个 load-bearing 岔路)

| 岔路 | 选项 | 决策 | 理由 |
|---|---|---|---|
| **范围 / 承诺** | 全量平价 vs 仅基础核心 vs 针对性子集 | **全量平价,分阶段** | 跨多 slice 承诺整个模型;第一个 slice = 基础核心;ROADMAP 开 §F 承接长尾。匹配项目的 incremental slice 节奏(slices 1–53 立先例)。 |
| **加载机制** | 静态注册 vs 动态 dylib vs Wasm vs out-of-process vs scripted | **静态先行,dylib 后补** | Phase 1:extension 是 Rust crate,经 `inventory::submit!` / 显式 `register_extension()` 注册,编译进来——最简、最安全、快速验证 trait 形状。Phase 2:`libloading` / `abi_stable` 从磁盘 dylib 加载,实现 pi-mono "drop file in dir" 平价。静态 trait 形状是稳定契约;dylib loader 后续包同一个 trait——**无 ABI churn**。 |
| **集成模型** | wrap 现有注册点 vs 新并行注册点 vs 替换现有 | **Wrap 现有注册点** | `registerTool` → `ToolRegistry`、`registerCommand` → `execute()` 在静态 match 前运行时 lookup、`registerProvider` → `ProviderRegistry::register`(已 last-wins = pi-mono `setProvider`)、`pi.on` → `HostAgentExecutor` seams。现有扩展点原样保留;extension 系统是新上层。匹配 pi-mono(它 wrap)与 §E 模式(框架 traits in core、adapter in agent-runtime、wiring in tui)。真正分阶段——每个 contribution point 包一个现有 registry,逐 slice。 |
| **Install 模型** | hot-load(无重启)vs install-then-reload vs 两者 | **Install + reload,永不 hot-load** | `/extension install` fetch/compile/place 到 `~/.codesmith/extensions/`;`/extension reload` 重新发现 + 重新加载(stale-context guard 失效旧 runtime)。永不 hot-load——reload 是 clean break,安全故事更简单。匹配 pi-mono 的 `/reload`。注意:这意味着**新 extension 的动态 install 是 phase-2(dylib)功能**——静态(phase 1)按定义无法在运行时加载新 extension;phase 1 对编译进来的 extension 提供 enable/disable/list/info/status/reload,`/extension install`/`uninstall` stub "phase 2"。 |

**所需操作(install 时 + 使用时):** 动态 **install**、**enable**、**disable**(observe 折进支撑性的 `list`/`info`/`status`——操作 enable/disable 需要,但非首要目标)。

---

## 3. 架构与 crate 布局

镜像 §E 三层模式:框架 traits in core、adapter/bridge in agent-runtime、host wiring in tui。**新增一个 crate。**

```
                         ┌──────────────────────────────────┐
                         │ codesmith-config                  │  extension 发现 config
                         └─────────────┬────────────────────┘
                                       │ dep
          ┌────────────────────────────┴───────────────────────────────┐
          ▼                                                              ▼
┌──────────────────────────────┐                       ┌─────────────────────────────────┐
│ codesmith-agent (CORE)       │   traits ───────────▶ │ codesmith-extensions (NEW)        │
│  • extension::Extension       │   ◀──── cfg           │  • ExtensionRunner (host runtime)│
│  • extension::ExtensionApi   │                       │  • loader + discovery + manifest │
│  • extension::ExtensionContext│                       │  • stale-context guard (gen ctr) │
│  • extension::ExtensionEvent  │                       │  • EventBus                     │
│  • extension::ToolDefinition  │                       │  • ExtensionApi impl (backs reg)│
│  • extension::CommandDefinition│                       │  • install-source abstractions  │
│  • extension::Handler         │                       └─────────────┬───────────────────┘
└──────────────┬───────────────┘                                       │
               │ path dep                                             │
               ▼                                                       │
┌──────────────────────────────┐                                       │
│ codesmith-agent-runtime      │◀──────────────────────────────────────┘
│  • ExtensionToolSpecAdapter  │   (extension tool → ToolSpec, 镜像 ToolSpecAdapter)
│  • extension-command bridge   │   (registerCommand → execute() 运行时 lookup)
│  • host_executor seam wiring  │   (pi.on 事件 → HostAgentExecutor seams)
└──────────────┬───────────────┘
               │ path dep
               ▼
┌───────────────────────────────────┐
│ codesmith-tui  (HOST / binary)     │
│  • build_extension_runtime()       │  (discover → reconcile w/ state → load → bind_core)
│  • ExtensionStateStore             │  (镜像 SkillStateStore; crates/tui/src/extension_state.rs)
│  • /extension command group         │
│  • bind runner → HostAgentExecutor │
└─────────────────────────────────────┘
```

**分层规则:** `codesmith-extensions` 依赖 `codesmith-agent`(traits),从不反向。`codesmith-tui` 依赖 `codesmith-extensions`(host 接线)。`codesmith-agent-runtime` 提供 adapter,把 extension 注册桥接到现有 `ToolSpec` / command dispatch / provider registry / `HostAgentExecutor` seams。这正是 §E 模式(`ToolSpecAdapter` / `CallbackBridge` / `SessionChatHistory`)。

**为什么 trait 契约住在 `codesmith-agent`(core):** `Extension` / `ExtensionApi` / `ExtensionContext` / `ExtensionEvent` 是框架级抽象(像 `Tool` / `Callback` / `AgentExecutor`)。任何 host 都能驱动 extension-aware agent loop 而不依赖 `codesmith-extensions`。`codesmith-extensions` 是 runtime 实现(loader / runner / discovery / `ExtensionApi` impl),不是契约。

---

## 4. Extension 契约(trait 草图)

> 下方签名是**定形用的草图**,非最终签名。最终签名在实施时(slice 1)决定。

```rust
// crates/agent/src/extension.rs  (CORE — host-agnostic traits)

/// 可扩展性单元。一个 extension 可贡献 tool、command、provider、
/// renderer、flag、shortcut、event handler。
/// 镜像 pi-mono 的 `ExtensionFactory`——`configure` 是 factory 体。
#[async_trait]
pub trait Extension: Send + Sync {
    fn metadata(&self) -> ExtensionMetadata;        // id, version, source_info
    async fn configure(&self, api: &mut dyn ExtensionApi)
        -> Result<(), ExtensionError>;
}

/// 交给 `configure` 的注册 + action 表面。load 阶段是 stub
/// (注册排队进 `pending_*`),`ExtensionRunner::bind_core` 时换真实现。
/// 镜像 pi-mono 的 `ExtensionAPI`。
#[async_trait]
pub trait ExtensionApi: Send + Sync {
    // --- contribution points(命令式注册,非声明式)---
    fn register_tool(&mut self, def: ToolDefinition) -> RegistrationResult;
    fn register_command(&mut self, def: CommandDefinition) -> RegistrationResult;
    fn register_provider(
        &mut self,
        factory: Arc<dyn ProviderFactory>,
    ) -> RegistrationResult;
    // defer 到后续 slice:
    //   register_shortcut / register_flag /
    //   register_message_renderer / register_entry_renderer

    // --- 生命周期事件订阅 ---
    fn on(&mut self, event: ExtensionEventKind, handler: Box<dyn Handler>);

    // --- 扩展间 pub/sub ---
    fn events(&self) -> &dyn EventBus;

    // --- action(bind_core 前是 stub)---
    async fn send_message(&self, msg: ExtensionMessage)
        -> Result<(), ExtensionError>;
    async fn append_entry(&self, entry: ExtensionEntry)
        -> Result<(), ExtensionError>;
    fn set_session_name(&self, name: &str) -> Result<(), ExtensionError>;
    // ... set_model / set_label / exec / compact /
    //     get_context_usage / get_system_prompt / ...

    /// Stale-context guard 的 generation 计数器。session reload/fork
    /// 前捕获的 handler 必须检查它;使用 stale `ctx` 报错。
    fn generation(&self) -> u64;
}

/// 传给每个 event handler。读为主;session 变更 action 住在
/// `ExtensionCommandContext`(子 trait,只交给 command handler)。
/// 镜像 pi-mono 的 `ExtensionContext`。
#[async_trait]
pub trait ExtensionContext: Send + Sync {
    fn cwd(&self) -> &Path;
    fn mode(&self) -> ExtensionMode;            // Tui | Rpc | Json | Print
    fn is_idle(&self) -> bool;
    fn signal(&self) -> CancellationToken;
    async fn abort(&self);
    async fn shutdown(&self);
    async fn compact(&self) -> Result<(), ExtensionError>;
    async fn get_context_usage(&self) -> ContextUsage;
    fn generation(&self) -> u64;               // stale-context 检查
    // ...
}

/// ~30 个生命周期事件。Phase 1 出最小集(见 §10.1);enum 是
/// `#[non_exhaustive]`(逐 slice 加变体),但 handler 分发是 open 的
/// (任何 `Handler` 可订阅任何变体)。镜像 pi-mono 的 `ExtensionEvent` union。
#[non_exhaustive]
pub enum ExtensionEvent {
    ProjectTrust { reason: TrustReason },
    SessionStart { reason: SessionReason },   // startup | reload | new | resume | fork
    ResourcesDiscover { reason: DiscoverReason },
    Input(InputEvent),                         // intercept / transform / handle
    BeforeAgentStart(AgentStartEvent),         // inject message, modify system prompt
    AgentStart, TurnStart,
    BeforeProviderHeaders, BeforeProviderRequest,
    AfterProviderResponse,
    ToolExecutionStart, ToolCall(ToolCallEvent),      // block
    ToolExecutionUpdate, ToolResult(ToolResultEvent), // modify
    ToolExecutionEnd, TurnEnd, AgentEnd, AgentSettled,
    SessionBeforeSwitch, SessionBeforeFork, SessionShutdown,
    SessionBeforeCompact, SessionCompact,
    // ...(~30 全集;slice 1 出最小集,其余按 §10.1 defer)
}
```

**从 pi-mono 保留的关键语义:**

- **命令式注册**(非声明式 manifest)——运行时 tool 注册与 provider swap 的前提。
- **两阶段构造**——`ExtensionApi` 在 load 时是 stub(注册排队进 `pending_*`);`ExtensionRunner::bind_core` 在 host 接线时换真实现(镜像 pi-mono `bindCore` 的 stubs→real swap)。
- **事件 cancel / transform / block 语义**——`BeforeAgentStart` 可注入消息或改 system prompt;`ToolCall` 可 block;`ToolResult` 可 modify;`SessionBefore*` 可 cancel。每个 handler 返回 result 类型;handler 链式(一个 handler 的修改对下一个可见)。
- **Stale-context guard**——`generation()` 计数器;`ExtensionRunner::invalidate()` 在 reload / session-replace / fork / switch 时递增;bump 前捕获的 `ctx` / `api` 使用即报 `ExtensionError::StaleContext`(镜像 pi-mono `runtime.invalidate()` / `assertActive()`)。

---

## 5. Contribution points 与 Wrap 映射

8 个 contribution point,每个 wrap 一个现有 CodeSmith 注册点。冲突解决逐字移植自 pi-mono(first-wins / `:N` 后缀 / 保留键位)→ `ResourceDiagnostic`,**不崩溃**。

| Contribution point | Wrap 目标(现有) | 冲突解决 | Phase |
|---|---|---|---|
| `registerTool` | `ToolRegistry` / `ToolSet`,经 `ExtensionToolSpecAdapter`(in `codesmith-agent-runtime`,镜像 `ToolSpecAdapter`) | 跨所有 extension first-wins | **slice 1** |
| `registerCommand` | `execute()` 在静态 `COMMANDS` match *之前*运行时 lookup(扩展现有 `commands/mod.rs` 的 user-defined-command 模式) | `:N` 后缀(`name`、`name:2`、…) | **slice 1** |
| `pi.on(event)`——最小集 | `HostAgentExecutor` seams(pre-request / post-stream / per-tool) | per-handler try/catch → `emit_error` | **slice 1** |
| `registerProvider` | `ProviderRegistry::register`(已 last-wins = pi-mono `setProvider`) | last-wins | slice 2+ |
| `pi.events`(`EventBus`) | 新 `EventBus` in `codesmith-extensions` | 命名空间 channel | slice 2+ |
| `registerShortcut` | TUI keybinding registry | 保留键位列表 | slice N |
| `registerFlag` | CLI flag registry | first-wins | slice N |
| `registerMessageRenderer` / `registerEntryRenderer` | transcript renderer registry | 每个 `customType` first-wins | slice N |

### 5.1 Slice-1 wrap 落点

1. **`registerTool` → `ToolRegistry`**:`ExtensionToolSpecAdapter`(in `codesmith-agent-runtime`,镜像 `ToolSpecAdapter`)实现 `ToolSpec`;其 `execute` 委托给 `ToolDefinition::execute`(异步,带 `ExtensionContext`)。注册时把 adapter 包进 `Arc<dyn ToolSpec>` 插入 `ToolRegistry`。**agent loop 不变**——它只看到 `ToolSpec`。

2. **`registerCommand` → `execute()`**:在 `commands/mod.rs::execute()` 加一个 `extension_commands::try_dispatch(app, cmd)` lookup,在 user-defined-command lookup **之后**、静态 `match` **之前**(复用 `user_commands::try_dispatch_user_command` 模式)。extension-command registry 是运行时可变 `HashMap<String, RegisteredCommand>`,由 `ExtensionRunner` 在 `bind_core` 时填充。现有 smoke test(每个 `COMMANDS` 条目 + alias 分派)自动扩展覆盖已注册的 extension command。

### 5.2 冲突 diagnostics

一个 `ResourceDiagnostic` 集合,bind 时一次性上报;不阻止其他 extension 加载(镜像 pi-mono `resource-loader.ts:570-577`)。

---

## 6. 生命周期与操作层(install / enable / disable)

### 6.1 Reload 序列(所有"动态"操作的核心;永不 hot-load)

```
/extension reload   (或 /extension install <src> 在 place 后自动触发)
  │
  ▼
1. invalidate()        — Arc<AtomicU64> generation 递增;handler 捕获的旧
                         ExtensionApi / ExtensionContext 现在使用即报
                         StaleContext(stale-context guard)
2. re-discover         — 遍历发现源(§7),与 ExtensionStateStore 的
                         enabled/disabled 标志 reconcile
3. re-load             — 遍历 enabled 集:
                           · phase 1(静态):inventory::iter::<Box<dyn Extension>>()
                             (编译进来)
                           · phase 2(dylib):libloading::Library::new(path)
                             → symbol "register_extension"
4. re-configure        — 对每个调 Extension::configure(&mut stub_api);
                         注册排队进 pending_*
5. bind_core           — stub ExtensionApi impl 换真实现;把 pending_*
                         flush 进 host 注册点(ToolRegistry / execute()
                         command lookup / ProviderRegistry);
                         emit SessionStart { reason: Reload }
```

### 6.2 `ExtensionStateStore`(镜像 `SkillStateStore`)

住在 `crates/tui/src/extension_state.rs`。TOML 在 `~/.codesmith/extensions_state.toml`:

```toml
# 发现但不加载的 extension
disabled = ["ext-id-1"]
# phase 2: install-source provenance 跟踪
installed = ["git:github.com/foo/bar@v1"]
```

- 默认空(全 enabled);损坏文件 → log + 默认处理,升级不会藏起所有 extension(匹配 `SkillStateStore` 策略)。
- 原子写(`tmp` + `rename`);backs `/extension enable|disable` + GET/POST runtime API。

### 6.3 `/extension` 命令组

住在 `crates/tui/src/commands/extension.rs`,经 `registerCommand` wrap 落点接入 `execute()`(§5.1.2——user-defined 之后、静态 `match` 之前的运行时 lookup)。

| 命令 | Phase 1(静态) | Phase 2(dylib) |
|---|---|---|
| `/extension list` | 编译进来 ext + state | + installed ext + provenance |
| `/extension info <id>` | contributions + generation + state | + install source/version |
| `/extension enable <id>` | 编译进来 | + installed |
| `/extension disable <id>` | 编译进来 | + installed |
| `/extension reload` | 重新发现 + 重新加载(编译进来) | + dylib 加载 |
| `/extension status` | runner 运行时状态(active exts、pending、dispatch 统计) | + loader 状态 |
| `/extension install <src>` | **stub**:报 "requires dylib loader (phase 2)" | fetch / compile / place + reload |
| `/extension uninstall <id>` | **stub**:报 "requires dylib loader (phase 2)" | remove + reload |

### 6.4 Install-source 抽象(phase 2,in `codesmith-extensions`)

```rust
pub trait ExtensionSource: Send + Sync {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError>;
}
// impl:GitSource(git clone --depth 1 --branch <ref>) /
//        CratesIoSource(.crate tarball) /
//        LocalPathSource /
//        PrebuiltDylibSource(直下载 .dylib)

pub trait ExtensionBuilder: Send + Sync {
    fn build(&self, src_dir: &Path) -> Result<PathBuf, ExtensionError>;  // → lib*.dylib
}
// impl:CargoBuilder(cargo build --release)

pub trait ExtensionPlacer: Send + Sync {
    fn place(&self, artifact: &Path) -> Result<PathBuf, ExtensionError>;  // → ~/.codesmith/extensions/<id>/
}
```

镜像 pi-mono 的 `package-manager.ts`。`/extension install <src>` 解析 source → fetch → build → place → 记入 `ExtensionStateStore.installed` → reload。

---

## 7. 发现、manifest、stale-context guard

### 7.1 Phase 1(静态)发现

- 编译进来的 extension 经 `inventory::submit! { ExtensionRegistration { factory: || Box::new(MyExt), metadata } }` 注册,或在 `codesmith-tui` build 接线里显式 `register_extension(|| Box::new(MyExt))`(镜像 pi-mono 的 `builtInExtensions` / inline factory)。
- `Cargo.toml` manifest(phase 1 仅元数据——加载不依赖它,但 `list` / `info` 显示用它,phase-2 dylib 发现复用同形状):

  ```toml
  [package.metadata.codesmith]
  extension = true        # 标记此 crate 是 codesmith extension
  id = "my-ext"            # 稳定 id(默认 crate name)
  ```

### 7.2 Phase 2(dylib)发现(镜像 pi-mono `discoverExtensionsInDir`)

- **三个源**(像 pi-mono):全局 `~/.codesmith/extensions/`、项目 `.codesmith/extensions/`、显式配置路径(`settings.extensions[]`)。
- **一层深**扫描:直接 `*.dylib` / `*.so` / `*.dll` 文件;带 `extension.toml` manifest 的子目录。
- **`extension.toml` manifest:**

  ```toml
  id = "my-ext"
  version = "1.0.0"
  entry = "libmy_ext.dylib"   # 或约定 <id>.<dylib-ext>
  [source]
  type = "git"                # provenance
  ref = "v1.0.0"
  ```

- **Trust prompt**:项目本地 `.codesmith/extensions/` 仅在 `project_trust` 事件后加载(镜像 pi-mono);全局 + 配置路径始终加载。首次项目信任 → 用户 prompted;按项目记忆。对齐现有 `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` 安全审计方向。
- 按 resolved path 去重;重复 first-wins。

### 7.3 Stale-context guard(phase 1+)

- `Arc<ExtensionRuntime>` 带 `generation: AtomicU64`。
- `invalidate()` 在 reload / session-replace / fork / switch 时递增 generation。
- `ExtensionApi::generation()` 与 `ExtensionContext::generation()` 暴露它。
- Handler / command 以 `Arc` clone 捕获 `api` / `ctx`;使用时比对 cached generation vs 当前 → 不匹配报 `ExtensionError::StaleContext`。
- 镜像 pi-mono `runtime.invalidate()` / `assertActive()`。因为**永不 hot-load**,reload 是 clean break(旧 runtime 失效)——比 hot-load 安全故事更简单,也是决策 §2.4 的直接回报。

---

## 8. 沙箱立场、API 版本、错误隔离

### 8.1 沙箱立场(显式,像 pi-mono)

- **Phase 1(静态,编译进来)**:extension 是 host crate——与 host 二进制同信任。无需沙箱;"只 ship 你信任的 extension"。
- **Phase 2(dylib)**:`unsafe` dylib 在进程内跑任意 native 代码。立场匹配 pi-mono:**"no sandbox, trust the source."** 安全委托给 (a) 项目本地 extension 的 trust prompt、(b) 全局源的用户判断、(c) 容器化模式(现有 `codesmith-sandbox` crate + `docs/SANDBOX.md` Docker / Gondolin 模式)处理不受信源。在 `docs/EXTENSIONS.md` 文档化此立场。
- **永不 hot-load**(决策 §2.4)→ reload 是 clean break(旧 runtime 失效)——比 hot-load 安全故事更简单,也是 install-model 决策的直接回报。

### 8.2 API 版本

- Lockstep `codesmith-agent` 版本(镜像 pi-mono lockstep)。
- extension crate 依赖 `codesmith-agent = "X.Y"`(trait 契约);semver-兼容的 host 加载它们。
- manifest 无 per-extension 声明的 API 版本(隐式由 dep 版本)。未来:crate 改名时加 back-compat alias(镜像 pi-mono `@mariozechner/*` → `@earendil-works/*`)。

### 8.3 错误隔离

- 每个 handler 调用包 `catch_unwind` + try;失败 → `ExtensionError` 收集,经 `/extension status` + diagnostics 上报,**不崩溃**。
- 冲突 diagnostics(first-wins / `:N` / 保留键位)在 `bind_core` 时上报,不崩溃(镜像 pi-mono `ResourceDiagnostic`)。
- 一个 extension 失败不阻止其他加载。

---

## 9. ROADMAP §F 大纲(长尾承诺)

本设计开一个新的 ROADMAP section。§F 长尾(slice 2 起,顺序指示性,非最终):

1. **§F1 — Slice 1(基础核心,phase 1 静态)。** 见 §10。
2. **§F2 — 完整生命周期事件集。** ~25 个剩余 `ExtensionEvent` 变体 + 跨 handler 的 cancel / transform / block 链;每个接到 `HostAgentExecutor` 的 seam。
3. **§F3 — `EventBus`(扩展间 pub/sub)。** 命名空间 channel;`pi.events` 表面;sample extension 演示 pub/sub。
4. **§F4 — `registerProvider`(运行时 provider 注册)。** 在 `bind_core` 把 `pending_provider_registrations` flush 进 `ProviderRegistry`;`unregisterProvider`;运行时 provider swap。
5. **§F5 — Dylib 加载(phase 2)。** `libloading` / `abi_stable` loader;`~/.codesmith/extensions/` 发现;`extension.toml` manifest;项目本地 trust prompt;`/extension install` / `uninstall` 真实现;install-source 抽象(Git / CratesIo / LocalPath / PrebuiltDylib)。
6. **§F6 — Renderer(`registerMessageRenderer` / `registerEntryRenderer`)。** 自定义 transcript message + TUI entry renderer;每个 `customType` first-wins。
7. **§F7 — `registerShortcut` + `registerFlag`。** TUI keybinding registry 带保留键位列表;CLI flag registry(first-wins)。
8. **§F8 — 嵌入 API。** 面向嵌入者的 `create_agent_session(options)` 等价物(编程式 `custom_tools`、inline extension factory、自定义 UI context)——镜像 pi-mono `sdk.ts`。

**Hot-load 明确永不进 §F**(决策 §2.4——仅 install + reload)。

---

## 10. Slice 1 范围与测试

### 10.1 Slice 1(基础核心,phase 1 静态)——落地内容

- **`codesmith-agent/src/extension.rs`**:`Extension` / `ExtensionApi` / `ExtensionContext` / `ExtensionCommandContext` / `ExtensionEvent` / `Handler` / `ToolDefinition` / `CommandDefinition` traits + 类型。**最小事件集:** `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` / `SessionShutdown`;enum 是 `#[non_exhaustive]`,其余在后续 slice 加而不破坏。
- **新 crate `codesmith-extensions`**:`ExtensionRunner`(事件分发 + 冲突解决 + stale-context guard,经 `Arc<AtomicU64>` generation) +
  `EventBus` skeleton +
  `ExtensionApi` stub→real impl(两阶段) +
  `inventory`-based 静态发现 +
  install-source 抽象(仅 trait,impl defer 到 §F5)。
- **`codesmith-agent-runtime`**:`ExtensionToolSpecAdapter`(extension `ToolDefinition` → `ToolSpec`,镜像 `ToolSpecAdapter`) +
  extension-command bridge(`try_dispatch_extension_command` in `commands/mod.rs::execute()`,user-defined 之后、静态 `match` 之前) +
  `HostAgentExecutor` seam wiring(事件 → pre-request / post-stream / per-tool seams)。
- **`codesmith-tui`**:`build_extension_runtime()`(经 `inventory` 发现编译进来的、与 `ExtensionStateStore` reconcile、load、configure、`bind_core`) +
  `ExtensionStateStore`(in `crates/tui/src/extension_state.rs`,镜像 `SkillStateStore`) +
  `/extension` 命令组(`list` / `info` / `enable` / `disable` / `status` / `reload` 工作;`install` / `uninstall` stub "phase 2")。
- **一个 in-tree sample extension**(编译进来,注册一个 tool + 一个 command + 一个 event handler)——验证载体。
- **ROADMAP §F1** 进度条目 + **`ARCHITECTURE.md`** 加 "Extension system" section,镜像 §E section 形状;**`docs/EXTENSIONS.md`** 撰写开发者指南 + 沙箱立场。

### 10.2 Slice 1 显式 defer(ROADMAP §F2–§F8 跟踪)

- **完整事件集**(~25 更多变体:`Input` / `BeforeAgentStart` / `BeforeProviderHeaders` / `BeforeProviderRequest` / `AfterProviderResponse` / `ToolExecutionStart` / `ToolExecutionUpdate` / `ToolExecutionEnd` / `AgentEnd` / `AgentSettled` / `SessionBeforeSwitch` / `SessionBeforeFork` / `SessionBeforeCompact` / `SessionCompact` / `ResourcesDiscover` / `ProjectTrust` / …) + cancel / transform / block 链。
- **`registerProvider`**(运行时 provider 注册)——§F4。
- **`registerShortcut` / `registerFlag` / `registerMessageRenderer` / `registerEntryRenderer`**——§F6 / §F7。
- **`EventBus` 完整 impl**——§F3(slice 1 只出 skeleton)。
- **Dylib 加载**(phase 2)+ `/extension install` / `uninstall` 真实现 + install-source impl + trust prompt + `extension.toml` manifest + 项目本地发现——§F5。
- **嵌入 API**(`create_agent_session`)——§F8。
- **Hot-load**——**永不**(决策 §2.4)。

### 10.3 测试策略

- **Unit:**
  - `ExtensionRunner` 事件分发——mock extension 订阅每个最小事件;断言 handler 触发 + cancel / transform / block 语义。
  - 冲突解决——两个 extension 注册同名 tool → first-wins + `ResourceDiagnostic`;同名 command → `:2` 后缀。
  - Stale-context guard——捕获 `ctx`、`invalidate()`、使用 → `ExtensionError::StaleContext`。
  - `ExtensionStateStore` round-trip + 损坏文件容错(匹配 `SkillStateStore` 测试形状)。
- **Integration:**
  - `ExtensionToolSpecAdapter` 端到端——extension 注册的 tool 经 `HostAgentExecutor` tool-call roundtrip(镜像 `framework_adapter.rs` tests)。
  - Extension-command dispatch——注册的 command 经 `execute()` 触发。
  - `EventBus` skeleton pub/sub(slice 1 skeleton;完整在 §F3)。
- **Smoke**(镜像 `commands/mod.rs` smoke tests):每个 `/extension` 子命令分派。
- **Sample extension**:in-tree sample 本身即测试(注册 + 运行)。

### 10.4 验证 gate(按项目惯例)

- `cargo +1.90.0 build --workspace` 绿。
- `cargo +1.90.0 test -p codesmith-extensions --lib` 绿(新 crate)。
- `cargo +1.90.0 test -p codesmith-agent --lib` 绿(trait 契约测试)。
- `cargo +1.90.0 test -p codesmith-agent-runtime --lib` 绿(adapter 测试,baseline 1149 pass + 2 ignored per slice 53)。
- `cargo +1.90.0 test -p codesmith-tui --bin` 绿(smoke + command 测试,baseline 2844 pass + 2 ignored per slice 52)。
- `grep` 验证:新代码无 stale "deferred" 标记;§F1 ROADMAP 条目在;`docs/EXTENSIONS.md` 在。

---

## 11. Open questions / 未来考虑

- **Slice 1 sample extension 形态**——in-tree sample 应注册什么以最大化验证架构?候选:一个 "scratchpad" extension,贡献一个 `/scratch` command + 一个 `scratch` tool + 一个 `TurnStart` handler 注入 context。在实施计划里定。
- **`ExtensionCommandContext` vs `ExtensionContext` 拆分**——确切的 session-变更 action 集只交给 command handler(镜像 pi-mono 拆分)。slice 1 定。
- **Event handler trait 形状**——单个 `Handler` trait 带 per-variant 关联类型,vs per-variant handler trait。pi-mono 用单个 `ExtensionHandler<E, R>` 泛型;Rust 惯用法可能偏好 per-variant trait 以求 exhaustiveness。slice 1 定。
- **`abi_stable` vs raw `libloading` for §F5**——`abi_stable` 给 ABI-stable vtable(跨编译器版本更安全),代价是更重依赖 + macro 驱动的 trait 形状;raw `libloading` 更轻但需手写 C-ABI 表面。决策 defer 到 §F5。
- **Install-source 优先级**——§F5 里哪些源是 must-have vs nice-to-have(git + local path 大概 must-have;crates.io + prebuilt-dylib URL nice-to-have)。§F5 实施计划定。
- **`docs/EXTENSIONS.md` 范围**——slice 1 撰写(开发者指南 + 沙箱立场)还是 defer 到专门 doc slice。建议:slice 1 撰写(项目惯例是落地 feature 同 slice 出 doc——见 slice 53 doc 对齐)。

---

## 附录 A — pi-mono 参考文件地图(移植 provenance)

实施时查阅这些 pi-mono 文件以获取 load-bearing 语义:

- `packages/coding-agent/src/core/extensions/types.ts`——完整契约。
- `packages/coding-agent/src/core/extensions/loader.ts`——发现 + jiti import + manifest + stub runtime(两阶段构造)。
- `packages/coding-agent/src/core/extensions/runner.ts`——host runtime、事件分发、冲突解决、`bind_core` 生命周期。
- `packages/coding-agent/src/core/extensions/wrapper.ts`——tool 到 agent core 的适配(`ExtensionToolSpecAdapter` 的类比)。
- `packages/coding-agent/src/core/agent-session.ts`(`_buildRuntime` + `reload`)——host 接线。
- `packages/coding-agent/src/core/resource-loader.ts`——trust-gated 发现 + 冲突 diagnostics。
- `packages/coding-agent/src/core/settings-manager.ts`——settings / packages schema。
- `packages/coding-agent/src/core/sdk.ts`——embed API(§F8)。
- `packages/coding-agent/docs/extensions.md`——完整参考(生命周期图在 275–345 行)。
- `packages/coding-agent/docs/packages.md`——npm/git package manifest + filtering(§F5 install-source 抽象)。
- `packages/coding-agent/examples/extensions/{hello,commands,tools,dynamic-tools,event-bus}.ts`
  与 `{with-deps,custom-provider-anthropic,sandbox,gondolin}/`——在 sample extension + test 中镜像的具体模式。
