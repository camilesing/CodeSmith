# CodeSmith Mods — Rhai 脚本 mod 层

Mods 用 [Rhai](https://rhai.rs) 脚本扩展 CodeSmith，不需要编译 Rust：写几行脚本就能给智能体加**事件钩子**（拦截/改写工具调用、输入、请求）、注册**模型可见工具**、注册**斜杠命令**，并用 **KV 存储**跨会话记住状态。写完激活即可在当前会话生效，改完保存自动热加载。

最简单的一个 mod 只需要 3 行脚本——下面这个会在执行包含 `rm -rf` 的命令前把它拦下来：

```rhai
on("tool-call", |e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        return block("rm -rf 需要人工确认");
    }
    proceed()
});
```

> 需要文件/网络访问或任意 Rust 生态能力？那是另一条路——Rust 扩展（dylib），见 [EXTENSIONS_cn.md](EXTENSIONS_cn.md)。两者共享同一套装配线，本教程只讲 Rhai Mods。English version: [MODS.md](MODS.md)。

## 五分钟上手

### 第 1 步：建目录，写两个文件

mods 放在全局根 `~/.codesmith/mods/<id>/`（所有工作区可见）或项目根 `<workspace>/.codesmith/mods/<id>/`（受工作区信任门控）。目录名就是 mod id：

```bash
mkdir -p ~/.codesmith/mods/hello
```

`~/.codesmith/mods/hello/mod.toml` —— 只有两个必填字段：

```toml
id = "hello"
version = "0.1.0"
```

`~/.codesmith/mods/hello/mod.rhai` —— 本页开头那 3 行守卫钩子。顶层脚本在加载时执行一次，`on(...)` 把钩子注册进运行时：

```rhai
on("tool-call", |e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        return block("rm -rf 需要人工确认");
    }
    proceed()
});
```

> `&&` 会短路：非 `exec_shell` 的工具调用不会走到 `e.input.command`，所以不必担心缺字段报错。

### 第 2 步：激活

在 CodeSmith 里敲：

```
/mods activate hello
```

**预期效果**：这是该 id 的**一次性同意**（mod 是进程内代码且跨会话持久，首次激活需要你确认）——确认后 mod 立即生效。此后同 id 的重载（含热载）不再需要审批。之后任何包含 `rm -rf` 的 shell 调用都会被拦截，模型会收到你的 reason 并改道。

### 第 3 步：注册一个模型可见工具 + KV 状态

在 `mod.rhai` 里追加：一个钩子统计每次工具调用，一个工具让模型查询计数。`mod_state_get/set` 是本 mod 私有的持久 KV（跨会话保留，mod 之间隔离）：

```rhai
on("tool-call", |e| {
    let n = mod_state_get("calls") ?? 0;   // 不存在时取 0
    mod_state_set("calls", n + 1);
    proceed()
});

register_tool(#{
    name: "call_count",
    description: "查询本次会话累计工具调用次数",
    schema: #{ type: "object", properties: #{}, additionalProperties: false },
}, |input| ok(mod_state_get("calls") ?? 0));
```

**预期效果**：下一个 turn 开始，模型的工具目录里出现 `call_count`；模型调用它时会拿到当前计数。`ok(...)` 构造成功结果（字符串原样返回，其他值 JSON 化）。

### 第 4 步：注册斜杠命令，改代码看热载

再追加一个斜杠命令（反引号字符串支持 `${...}` 插值）：

```rhai
register_command("calls", "显示工具调用次数", |args, ctx| {
    message(`到目前为止共调用 ${mod_state_get("calls") ?? 0} 次工具`);
});
```

**预期效果**：

- 敲 `/calls` 显示计数——`message(...)` 展示给用户（想触发智能体则用 `send(...)`）
- 现在**直接改 `mod.rhai` 并保存**——watcher 监视两个 mods 根（500ms 防抖 + 1s 冷却），安静后自动重载，同 id 免审批。`/mods status` 可确认已加载的 mod 与 runner 代数

到此你已经用过全部四种注册面：事件钩子、工具、命令、KV。完整 `/mods` 命令：`list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`。

## 速查

### 事件钩子

`on("<event>", |e, ctx| { ... })`——`e` 是事件 payload map（都带 `kind` 字段），`ctx` 是 `#{cwd, mode, idle, generation}`，**ctx 可省略**（写 `|e|` 即可）。事件名是 `ExtensionEventKind` 的 kebab-case 全量 23 种：

| 事件 | payload 字段 |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`、`inject_message`（`()` 表示未设置） |
| `before-provider-request` | `messages`（JSON 值） |
| `after-provider-response` | `response`（JSON 值） |
| `tool-call` | `id`、`name`、`input`（JSON 值） |
| `tool-result` | `id`、`name`、`content`、`success`、`is_error` |
| `turn-start` / `turn-end` | `turn_id`；`turn-end` 另有 `reason` |
| `tool-execution-update` | `id`、`name`、`message` |
| `project-trust` / `session-start` / `resources-discover` | `reason` |
| `agent-start` / `before-provider-headers` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` / `session-before-compact` / `session-compact` | （仅 `kind`） |

未接线事件（宿主 seam 尚未兑现）订阅不触发：`tool-execution-update`、`resources-discover`、`session-before-fork`。

分发模式是每个事件契约的一部分（`ExtensionEventKind::dispatch_mode`，由契约测试锁定）：

- **transform-chain**：`input`、`before-agent-start`、`before-provider-request`、`tool-result` —— transform 逐个折叠、后续 handler 可见、终值作用于宿主操作
- **cancel veto**：`session-before-switch`、`session-before-fork`、`session-before-compact`
- **block deny**：`tool-call`
- **observe**（outcomes 仅供参考）：其余全部

### 钩子返回值 → HandlerOutcome

| 脚本返回 | HandlerOutcome | 生效 seam |
|---|---|---|
| `proceed()` 或 `()` | `Continue` | 全部 |
| `block(reason)` | `Block`（越权 seam 忽略） | `tool-call` |
| `cancel(reason)` | `Cancel`（越权 seam 忽略） | `session-before-*` |
| `transform(#{...})` | `Transform`（合并可变字段后继续链） | 见下 |

Transform 可变字段（一个 handler 的改写对后续 handler 立即可见）：

- `input`：`text`
- `before-agent-start`：`system_prompt`、`inject_message`（字符串=设置，`()`=清除，缺省=保持）
- `before-provider-request`：`messages`
- `tool-result`：`content`、`success`、`is_error`

### 能力面函数

| 函数 | 说明 |
|---|---|
| `mod_state_get(key)` | 读持久 KV；不存在返回 `()`（配 `??` 默认值） |
| `mod_state_set(key, value)` | 写持久 KV（原子落盘，`~/.codesmith/mods-state/`） |
| `mod_log(msg)` | 打日志（target `codesmith_mods`） |
| `now_ms()` | Unix 毫秒时间戳 |
| `proceed / block / cancel / transform` | 钩子控制值 |
| `ok(value) / err(msg)` | 工具结果构造 |
| `message(msg) / send(msg)` | 命令输出（展示 / 注入对话） |
| `register_provider(spec)` | 注册 provider 别名（见下） |

### 注册 provider（路线 A）

Mod 可以注册一个 **provider 别名** —— 一个新的 provider id，委托给某个
内置 provider，并携带自己的 `base_url` / `default_model` / `headers`
覆盖。脚本无法实现 LLM 客户端（按设计无 async/网络），因此 mod 的
provider 贡献永远是这种别名，例如指向一个 OpenAI 兼容网关：

```rhai
register_provider(#{
    id: "acme-gw",                        // 新 id；不得遮蔽内置名称
    kind: "openai",                       // 委托到的内置 provider
    base_url: "https://gw.example.test/v1",
    default_model: "acme-large",
    headers: #{ "X-Gateway": "acme" },    // 可选
});
```

校验在 mod **加载**时即失败：`id` 遮蔽内置名称、`kind` 不是内置
provider、或 header 值不是字符串 —— 坏的 spec 到不了客户端。刻意不
接受 `api_key`：秘密只存在于配置中，绝不进入脚本。

选用别名：在 config.toml 声明一个同 `id` 的 `[[providers.custom]]`
条目（承载 API key），并以 `custom_provider = "acme-gw"` 选中。别名的
`base_url` / `default_model` / `headers` 覆盖在客户端构建时叠加于条目
值之上。注册会写日志（target `codesmith_extensions`），在下一次客户端
解析时生效（新会话、切换 provider）—— 运行中的会话沿用当前客户端。
卸载 mod 或 `/extension reload` 即移除别名。

### 资源限制与错误姿态

每次脚本调用上限 200,000 操作数、64 层调用深度、8 MiB 字符串 / 100k 数组与 map 元素。超限或脚本出错：**钩子 fail-open**（`warn` + `Continue`，一个坏 mod 不会打断链）；工具/命令返回普通错误给模型。不暴露文件/网络/进程——引擎未注册任何 I/O native，不注入即不存在。

## mod.toml 字段

| 字段 | 必需 | 说明 |
|---|---|---|
| `id` | ✓ | 稳定标识，`[a-zA-Z0-9._-]`，即目录名/状态键 |
| `version` | ✓ | 语义化版本串（展示用） |
| `name` | | 人类可读名，默认 = id |
| `description` | | 激活审批时展示给用户的一句话说明 |
| `entry` | | 入口脚本相对路径，默认 `mod.rhai`；绝对路径与 `..` 被拒绝 |

清单校验为 schema 聚合式：所有字段问题连同路径一次性报出（如
`version: missing required field; name: expected a string, got integer`），
而非遇错即停。未知字段警告后忽略（向前兼容，不拒绝）。清单校验失败的
mod 在发现阶段即被跳过（日志有告警），不会进入激活流程。

## 生命周期与安全模型

- **首次激活需确认**：新 mod 被发现 → 跳过加载 + TUI 被动提示待激活。激活只有两条路：`/mods activate <id>`，或审批 `manage_mods(action="activate")` 工具调用。激活记录持久化（`~/.codesmith/mods_state.toml`），同 id 重载免审批。**为什么**：mod 是进程内代码且跨会话持久——提示注入可以在用户无感时植入常驻钩子，首次确认正是防这一点。
- **让智能体代写**：直接说"帮我写一个 mod，拦截 git push"——模型会经 `manage_mods` 工具写文件（`write`）并请求你审批激活（`activate`）。
- **项目级 mods** 沿用 workspace trust：未信任工作区直接不发现；`manage_mods` 写项目 mod 同样拒绝未信任工作区。
- **已知边界**：mod 工具与 Rust 扩展工具同为"主 turn 独占"（子代理结构性不可见）；网络安装源（git clone 到 mods 目录）不在 MVP。

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

## 延伸阅读

- [EXTENSIONS_cn.md](EXTENSIONS_cn.md) — Rust 扩展（dylib / 编译进二进制）：同一 `Extension` 契约的完整能力形态
- [HOOKS_cn.md](HOOKS_cn.md) — shell 命令形态的生命周期钩子（进程外，与本页进程内钩子互补）
