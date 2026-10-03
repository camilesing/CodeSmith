# CodeSmith Mods — Rhai 脚本 mod 层作者指南

Mods 用 [Rhai](https://rhai.rs) 脚本扩展 CodeSmith：注册**事件钩子**、**模型可见工具**、**斜杠命令**，并携带跨会话持久的 **KV 状态**。不需要编译 Rust——写完即可在当前会话激活。

 Mods 与 Rust dylib 扩展（见 [EXTENSIONS.md](EXTENSIONS.md)）实现同一 `Extension` 契约、跑同一条 `ExtensionRunner` 装配线（7 个宿主 seam、信任门、代际失效热重载全部复用）；区别只在扩展载体是脚本而非 cdylib。

```
~/.codesmith/mods/<id>/            全局 mod 根（所有工作区可见）
<workspace>/.codesmith/mods/<id>/  项目 mod 根（受 workspace trust 门控）
        mod.toml                   清单（必需）
        mod.rhai                   入口脚本（默认名，可在 mod.toml 改）
```

## 快速上手

```bash
mkdir -p ~/.codesmith/mods/commit-guard
```

`~/.codesmith/mods/commit-guard/mod.toml`：

```toml
id = "commit-guard"
name = "Commit Guard"            # 可选，默认 = id
version = "0.1.0"
description = "拦截危险 git 操作"  # 可选，激活审批时展示给用户
entry = "mod.rhai"               # 可选，默认 mod.rhai；禁绝对路径与 ..
```

`mod.rhai`：

```rhai
// 顶层脚本在加载时执行一次（= configure）
on("tool-call", |e, ctx| {              // e = 事件 map；ctx = {cwd, mode, idle, generation}，可省略
    if e.name == "exec_shell" && e.input.command.contains("git push") {
        return block("push 前需确认");
    }
    proceed()                            // 放行；返回 () 等价
});
on("input", |e| {
    if e.text.starts_with("!") { return transform(#{ text: e.text.substring(1) }); }
    proceed()
});
register_tool(#{
    name: "team_ci_status",
    description: "查询团队 CI 状态",
    schema: #{ type: "object", properties: #{}, additionalProperties: false },
}, |input, ctx| {
    ok(mod_state_get("ci") ?? "unknown")   // ok()/err() 构造工具结果
});
register_command("ci", "显示 CI 状态", |args, ctx| {
    message(mod_state_get("ci") ?? "unknown")   // message()/send() 对应命令输出两变体
});
```

激活（一次性同意）：

```
/mods activate commit-guard
```

之后每次保存 `mod.rhai`/`mod.toml`，watcher 自动热重载（500ms 防抖 + 1s 冷却），无需再审批。

也可以直接让智能体代写：对话里说"帮我写一个 mod，拦截 git push"，模型会调用 `manage_mods(action="write", ...)` 写文件、`action="activate"` 请求你审批。

## mod.toml 字段

| 字段 | 必需 | 说明 |
|---|---|---|
| `id` | ✓ | 稳定标识。`[a-zA-Z0-9._-]`，即目录名/状态键 |
| `version` | ✓ | 语义化版本串（展示用） |
| `name` | | 人类可读名，默认 = id |
| `description` | | 激活审批时展示给用户的一句话说明 |
| `entry` | | 入口脚本相对路径，默认 `mod.rhai`。绝对路径与 `..` 被拒绝 |

## 事件钩子

`on("<event>", |e, ctx| { ... })`。`e` 是事件 payload map（各字段见下表）；`ctx` 是 `#{cwd, mode, idle, generation}`（可省略）。

事件名 = `ExtensionEventKind` 的 kebab-case 全量 23 种：

`project-trust` · `session-start` · `resources-discover` · `input` · `before-agent-start` · `agent-start` · `turn-start` · `before-provider-headers` · `before-provider-request` · `after-provider-response` · `tool-execution-start` · `tool-call` · `tool-execution-update` · `tool-result` · `tool-execution-end` · `turn-end` · `agent-end` · `agent-settled` · `session-before-switch` · `session-before-fork` · `session-shutdown` · `session-before-compact` · `session-compact`

未接线事件（宿主 seam 尚未兑现）订阅不触发，与 dylib 扩展一致：`tool-execution-update`、`resources-discover`、`session-before-fork`。

主要 payload 字段（所有事件都带 `kind` = 事件名）：

| 事件 | 字段 |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`（`()` 表示未设置）、`inject_message`（同） |
| `before-provider-request` | `messages`（JSON 值） |
| `after-provider-response` | `response`（JSON 值） |
| `tool-call` | `id`、`name`、`input`（JSON 值） |
| `tool-result` | `id`、`name`、`content`、`success`、`is_error` |
| `turn-start` / `turn-end` | `turn_id`；`turn-end` 另有 `reason` |
| `session-start` | `reason`（`startup`/`reload`/`new`/`resume`/`fork`） |
| 其余 | 仅 `kind` |

## 钩子返回值 → HandlerOutcome

| 脚本返回 | HandlerOutcome | 生效 seam |
|---|---|---|
| `proceed()` 或 `()` | `Continue` | 全部 |
| `block(reason)` | `Block`（越权 seam 忽略） | `tool-call` |
| `cancel(reason)` | `Cancel`（越权 seam 忽略） | `session-before-*` |
| `transform(#{...})` | `Transform`（按事件种类合并可变字段后继续链） | `input` / `before-agent-start` / `before-provider-request` / `tool-result` |

Transform 可变字段：

- `input`：`text`
- `before-agent-start`：`system_prompt`、`inject_message`（传字符串设置，传 `()` 清除，缺省保持）
- `before-provider-request`：`messages`
- `tool-result`：`content`、`success`、`is_error`

一个 handler 的 transform 对后续 handler 立即可见（与 Claude Mods 的 `next({...e, changed})` 语义同构）。

## 工具与命令

```rhai
register_tool(#{ name, description, schema }, |input, ctx| { ... })
```

- `name` 必须匹配 `[a-zA-Z0-9_-]{1,64}`；`schema` 是 JSON Schema 的 Rhai map 形态（缺省 `{"type":"object"}`）。
- 返回 `ok(value)`（成功，字符串原样、其他值 JSON 化）或 `err(msg)`；直接返回字符串视为成功内容。

```rhai
register_command("name", "描述", |args, ctx| { ... })
```

- `args` 是字符串；返回 `message(s)`（展示给用户）或 `send(s)`（注入对话、触发智能体）；直接返回字符串视为 `message`。

## 能力面与资源限制

**暴露的函数**：

| 函数 | 说明 |
|---|---|
| `mod_state_get(key)` | 读持久 KV；不存在返回 `()`（配 `??` 默认值） |
| `mod_state_set(key, value)` | 写持久 KV（原子落盘） |
| `mod_log(msg)` | 打日志（target `codesmith_mods`） |
| `now_ms()` | Unix 毫秒时间戳 |
| `proceed/block/cancel/transform/ok/err/message/send` | 控制值构造 |

**不暴露**：文件、网络、进程——Rhai 引擎未注册任何 I/O native，不注入即不存在（沿用 §F"信源+能力"立场）。后续按 manifest 权限声明再开。

**资源限制**：每次脚本调用上限 200,000 操作数、64 层调用深度、8 MiB 字符串 / 100k 数组与 map 元素。超限即中止脚本：钩子 fail-open（`warn` + `Continue`，走 `emit` 现有隔离语义，一个坏 mod 不会打断链）；工具/命令则返回普通错误。

**KV 隔离**：每个 mod 一个文件 `~/.codesmith/mods-state/<scope>-<id>.json`（scope = `global`/`project`），物理隔离，mod 之间互不可见。

## 生命周期与安全模型

- **首次激活需确认**：新 mod 被发现 → 跳过加载 + TUI 被动提示待激活。激活只有两条路：用户敲 `/mods activate <id>`，或审批 `manage_mods(action="activate")` 工具调用。激活记录持久化（`~/.codesmith/mods_state.toml`），同 id 重载（含 watcher 触发）免审批。
  **为什么**：mod 是进程内代码且跨会话持久——提示注入可以在用户无感时植入常驻钩子，首次确认正是防这一点。
- **项目级 mods** 沿用 workspace trust：未信任工作区直接不发现。
- **manage_mods 审批面**：`list`/`reload` 自动放行；`write`/`activate`/`disable`/`enable`/`remove` 一律要求用户审批。
- **已知边界**：mod 工具与 dylib 扩展工具同为"主 turn 独占"（子代理结构性不可见）；网络安装源（git clone 到 mods 目录）不在 MVP，二期挂到现有 `Installer` 体系。

## `/mods` 斜杠命令

```
/mods list                     列出已发现 mod 与激活状态
/mods status                   运行态：已加载/待激活/已禁用 + runner 代数
/mods info <id>                单个 mod 清单详情
/mods activate <id>            首次激活（一次性同意）+ 立即重载
/mods enable|disable <id>      开关（保留激活记录）
/mods remove <id>              删除目录 + KV + 状态
/mods reload                   手动重载整个扩展层
```

## 配置

`config.toml` 的 `[mods]` 节（两者默认 true）：

```toml
[mods]
enabled = true   # 总开关：发现、manage_mods 工具、watcher
watch = true     # 仅文件 watcher（500ms 防抖 + 1s 冷却）
```

## 实现索引

| 部件 | 位置 |
|---|---|
| `ModManifest` / `discover_mods` / 信任门 | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod`（Extension 实现、native 注册、事件映射） | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ModKvStore`（每 mod 持久 KV） | `crates/extensions/src/script/kv.rs` |
| `ModStateStore`（激活/禁用状态） | `crates/tui/src/mod_state.rs` |
| 共享操作层（/mods 与 manage_mods 单一实现、watcher） | `crates/tui/src/mod_ops.rs` |
| 装配/门控接入（`populate` 返回 pending 报告） | `crates/tui/src/core/engine.rs` |
| `ManageModsTool`（模型可见） | `crates/tui/src/tools/mods.rs` |
| `/mods` 命令 | `crates/tui/src/commands/mod_commands.rs` |
