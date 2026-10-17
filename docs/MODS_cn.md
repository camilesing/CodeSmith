# CodeSmith Mods — Rhai 脚本 mod 层

Mods 用 [Rhai](https://rhai.rs) 脚本扩展 CodeSmith，不需要编译 Rust：写几行脚本就能给智能体加**事件钩子**（拦截/改写工具调用、输入、请求）、注册**模型可见工具**、注册**斜杠命令**，并用 **KV 存储**跨会话记住状态。写完激活即可在当前会话生效，改动保存后重载即生效且同 id 免审批。

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

## 该用哪一层（可插拔表面地图）

所有可插拔能力都走同一个契约：`Extension`
（`crates/agent/src/extension.rs`）+ `ExtensionRunner` 装配线。实现它的有四层
——用户 Rhai mod、内建 Rhai 层（内嵌脚本默认值）、编译内置 Rust 扩展、安装的
Rust dylib——而"可插拔"的含义是*在运行期经其中一层注册*，而不是编译期焊死在
二进制里。各表面适配哪一层：

| 表面 | Rhai mod | 内建 Rhai 层 | Rust 扩展 | 备注 |
|---|---|---|---|---|
| 事件钩子、工具守卫 | ✓ `on` / `register_guard` | — | ✓ | 按卸载/加载粒度为 mod；钩子错误 fail open |
| 模型可见工具 | ✓ `register_tool`（纯逻辑——无 fs/net 原生函数） | ✓（可脚本化的默认值） | ✓（完整 IO/异步） | 所有层都仅限主轮 |
| 斜杠命令 | ✓ `register_command`（遮蔽内建） | — | ✓ | 层序 user > mod > builtin |
| Provider | 仅别名（`register_provider`） | — | ✓ 完整 `ProviderFactory` | 脚本无法实现 `LlmClient`（无异步/网络） |
| 提示词分段、技能 | ✓ `register_prompt_section` / `register_skill` | ✓（默认值） | ✓ | 遵守前缀缓存纪律 |
| 策略（宿主机制旋钮） | ✓ `register_policy`（last-wins） | ✓——默认值本身就是内建 mod | ✓ | 数据而非代码；`/mods list` 显示解析结果 |
| turn 循环、压缩算法、`BASE_PROMPT`、子代理工具注册表 | — | — | — | 契约声明的核心（唯一 turn 循环、唯一基础提示词、子代理注册表仅内建） |

经验法则：**逻辑与数据 → Rhai**（免重编译、consent 门控、以"无 I/O 原生
函数"为沙箱）；**异步、文件、网络或完整生态 → Rust 扩展**（dylib 安装 +
`/extension reload`）；**为既有宿主机制提供新默认值 → 内建 Rhai 层**（随
二进制分发，用户 mod 按种类覆盖）。这一分层是声明式架构：新的可插拔工作
落在其中一层上，而不是新增一个编译期接线点。

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

在 REPL 里用 `/mods activate hello` 激活——这是对该内容的一次性
同意（mod 是进程内代码且跨会话持久，首次激活是显式动作）。也可以
让智能体去激活——"激活 hello 这个 mod"——然后审批它发出的
`manage_mods(action="activate", id="hello")` 工具调用。

**预期效果**：激活后 mod 立即生效。此后同 id 的重载不再需要审批；但若入口文件在你批准后发生了变更（git pull、编辑），该 mod 会回到待激活状态，需要重新激活。之后任何包含 `rm -rf` 的 shell 调用都会被拦截，模型会收到你的 reason 并改道。

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

### 第 4 步：注册斜杠命令，重载改动

再追加一个斜杠命令（反引号字符串支持 `${...}` 插值）：

```rhai
register_command("calls", "显示工具调用次数", |args, ctx| {
    message(`到目前为止共调用 ${mod_state_get("calls") ?? 0} 次工具`);
});
```

**预期效果**：

- 在 REPL 里敲 `/calls some args` 即经扩展运行时分发：`message(...)` 直接打印文本，`send(...)` 将内容送入会话。mod 命令位于用户命令与内建表之间（用户命令可遮蔽它们，它们可遮蔽内建命令），`/help` 的 "mod commands" 节会列出，Tab 补全实时提供其名字。
- 现在**改 `mod.rhai`、保存，然后重载**——`/mods reload` 会重新执行发现流程，同 id 免审批。`/mods status` 确认哪些 mod 已加载及运行时代数（`/mods list` 显示各 mod 的激活状态与已解析策略层；启动提示也会列出已发现但未激活的 mod）。（自动文件 watcher 及其 `[mods] watch` 开关已在 0.9.0 移除，重载改为显式触发。）

到此你已经用过全部四种注册面：事件钩子、工具、命令、KV。`/mods` 的完整命令集为 `list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`；模型侧 `manage_mods` 工具与之对应，动作为 `list` / `write` / `activate` / `disable` / `enable` / `remove` / `reload`，另有会话实验线 `session_write` / `session_remove`（见下文）。

## 速查

### 事件钩子

`on("<event>", |e, ctx| { ... })`——`e` 是事件 payload map（都带 `kind` 字段），`ctx` 是 `#{cwd, mode, idle, generation}`（`mode` 在交互终端 REPL 下为 `"tui"`——沿用已移除前端的名字以保兼容——其余为 `"rpc"` / `"json"` / `"print"`），**ctx 可省略**（写 `|e|` 即可）。事件名是 `ExtensionEventKind` 的 kebab-case 全量 24 种：

| 事件 | payload 字段 |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`、`inject_message`（`()` 表示未设置） |
| `before-provider-request` | `messages`（JSON 值） |
| `after-provider-response` | `response`（JSON 值） |
| `tool-call` | `id`、`name`、`input`（JSON 值） |
| `tool-result` | `id`、`name`、`content`、`success`、`is_error`、`canonical`（JSON 值，工具未产出时为 `()`） |
| `turn-start` / `turn-end` | `turn_id`；`turn-end` 另有 `reason` |
| `assistant-stream` | `text`（单个增量块；随文本 delta 逐次触发，仅观察） |
| `tool-execution-update` | `id`、`name`、`message` |
| `tools-change` | `added`、`removed`（工具名数组） |
| `project-trust` / `session-start` | `reason` |
| `agent-start` / `before-provider-headers` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` / `session-before-compact` / `session-compact` | （仅 `kind`） |

`tool-execution-update` 由 shell 工具自己的轮询循环在命令持续运行时发射（`exec_shell`
前台与 `exec_shell_wait`）：至多每秒一次、且仅当有新输出。`id` 是 shell 任务 id（工具
不知道自己的 wire tool-call id——按 `name` + 时序关联）；`message` 是自上一检查点以来的
输出（尾部截至 4 KiB；管理器给出截断摘录时取其尾部）。仅观察、仅主回合（子代理循环
不发射扩展事件）；沙箱后端与交互式运行不报检查点。同一检查点也以 `ToolCallProgress`
到达 UI。`resources-discover` 已在 0.11.0 移除（从未有发射方，也没有消费者）。
`session-before-fork` 在每次 spawn 前触发——spawn 工具族（`agent_open` / `agent_spawn` /
`subagent_run` / `delegate_to_agent`）在任意深度（runner 随子代理运行时仅作否决用途传递，
故孙代 fork 同样可否决）以及引擎内部 spawn 路径——在 fork 会话状态前触发；`cancel()` 否决
该次 fork。子代理注册表仍不挂任何扩展工具：runner 仅为否决而携带。

`tool-result` 载荷携带 canonical/rendered 分离：`content` 是模型可见的渲染文本；`canonical` 是同一结果的结构化机器可读值（`ToolResult::json` 的值，或工具经 `with_canonical` 附加的值）。请读 `canonical` 而不是重新解析 `content`——散文式渲染的结果（如 `todo_add` 在 JSON 前拼了一句提示）根本无法解析。canonical 仅存活于当前进程：转录持久化的是渲染后的 `content`。

`tools-change` 在回合边界触发：当编译后的模型可见目录相对上一主回合发生变化（模式切换、mod 重载、选择变更）。首回合只建立基线、不触发；sub-agent 工具集不会触发；没有读取当前目录的 API——事件只携带差量。`/tools` 按来源（builtin / plugin / extension / mcp）分组展示当前基线。

分发模式是每个事件契约的一部分（`ExtensionEventKind::dispatch_mode`，由契约测试锁定）：

- **transform-chain**：`input`、`before-agent-start`、`before-provider-request`、`tool-result` —— transform 逐个折叠、后续 handler 可见、终值作用于宿主操作
- **transform + deny**：`tool-call` —— 改写调用 `input`（改写后的输入才是审批与实际执行的输入，也是入档输入），或 `block(reason)` 拒绝（单调——后续 handler 不可翻转）
- **cancel veto**：`session-before-switch`、`session-before-fork`、`session-before-compact`
- **observe**（outcomes 仅供参考）：其余全部，含 `assistant-stream`（随文本 delta 逐次触发——handler 须轻量）与 `tools-change`（回合分发收口的目录差量）

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
- `tool-result`：`content`、`success`、`is_error`、`canonical`（JSON 值——结果的机器可读值；仅改写 `content` 不动它，可显式替换）

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
| `register_prompt_section(id, text)` | 向基础系统提示词追加命名分段（见下） |
| `register_skill(spec)` | 向会话技能目录贡献一个内存技能（见下） |
| `register_guard(callback)` | 注册 deny-only 的工具调用守卫（见下） |
| `register_message_projection(key, init, fold)` | 注册由宿主维护的会话日志折叠（见下） |
| `projection_state(key)` | 读取本 mod 的投影状态（钩子/工具内可用） |
| `register_policy(kind, config)` | 覆盖宿主机制策略（见下） |

### 策略（路线 C —— 调优宿主机制）

```rhai
register_policy("shell-output", #{
    max_output_size: 20000,   // 触发截断的字节数
    head_bytes: 15000,        // 截断时保留的头部
    tail_bytes: 5000,         // 截断时保留的尾部
});
```

策略是**数据而非代码**：`kind` 指名一个宿主机制接缝，`config` 是它的
参数表；机制本身（算法、溢写、安全守卫）留在 Rust。这就是"策略在脚本、
效果在宿主"的切分——agent 无需重编译即可按工作区读取并调整这些行为。

- 种类：`output-diff`（`context_radius`）、`shell-output`
  （`max_output_size`、`head_bytes`、`tail_bytes`、`summary_max_lines`、
  `summary_max_chars`、`preserved_summary_lines`、`preserved_line_bytes`、
  `preserved_total_bytes`）、`compact-policy`（`cache_trigger_threshold`、
  `time_trigger_gap_secs`、`micro_compactable_tools`、
  `cleared_placeholder` —— 压缩引擎本体留在 Rust；摘要提示词由多个会话
  来源合并而成、并非单一模板，故不进此接缝）、`large-output-route`
  （`threshold_tokens`、`per_tool` —— 路由决策；utility LLM 综合调用留在
  Rust。显式 `[workshop]` TOML（`large_output_threshold_tokens` /
  `per_tool_thresholds`）位于该种类之上：config 值优先，TOML 未设时由
  脚本层提供默认值）。
- 解析为**按装载顺序后写者胜**（内置 < 全局 < 项目）：用户 mod 注册同
  种类即覆盖内置默认——与工具/命令的"先到者得"刻意不同。
- **可观测**：`/mods list` 与 `manage_mods action=list` 末尾附带"已解析
  策略"节——每个种类、其生效值、获胜 mod 的 id（内置条目显示
  `builtin-*` id）。卸载/禁用获胜 mod 后该节实时回落到下一获胜者，
  agent 可借此自证其策略覆盖。
- 非法取值（字段类型错误、超上限、head+tail > max）在读取时告警并回退
  到编译默认——mod 的 bug 不能瘫痪它所调优的机制。
- **内置层**：默认值本身以内嵌 Rhai mod 发布（`builtin-output-diff`、
  `builtin-shell-output`、`builtin-compact-policy`、
  `builtin-large-output-route` —— 源码在 `crates/extensions/src/script/builtin/`）。
  它们始终装载（`[mods] enabled = false` 也只禁用户 mod）、跳过同意/哈希
  门（随二进制发布）、使用临时 KV，且不出现在 `/mods` 的 mod 列表（不是
  可发现 id —— 覆盖方式是注册同种类策略，而非遮蔽 id；其 id 仅作为
  `list` 已解析策略节的来源出现）。

### 会话级临时 mod（当会话实验）

```
manage_mods {"action": "session_write", "id": "session-shell-tuning",
             "source": "register_policy(\"shell-output\", #{ max_output_size: 20000 });"}
```

`manage_mods` 的 `session_write` 用内联 Rhai 源码直接绑定一个 agent 编写的
实验 mod——不写文件、无激活状态、KV 临时（内置层的编译路径 + 运行时提供的
源码）。它服务的闭环：本会话内试一个 mod/策略 → 用 `list` 验证 → 验证有效
再经 `write` + `activate` 持久化。

- id 必须带 `session-` 前缀、不得与磁盘发现的 mod 冲突；同 id 重写为无
  残留替换。
- 同意：同一 `(id, source)` 首次写入需审批；**本会话内完全相同的重写免
  复审**（activation hash 的会话级类比）——内容一变重新询问。
  `session_remove` 恒需审批。
- 位置：session mod 绑定在磁盘层之后，其策略注册在后写者胜中压过一切
  （已解析策略节显示 `from session-*`）。
- 生命周期：会话结束即消失（任何东西都不持久化，包括 KV）。`/mods
  reload` 与 `manage_mods action=reload` 会在磁盘层之后重放它们（保留的
  编译形态，无重编译）。会话恢复不重建。其策略进程全局生效（子代理的
  机制读同一快照）；其工具/命令不进入子代理注册表（设计上仅内建）；
  fork 否决覆盖所有代际。
- 列表显示为 `session-* [session] — live (session-scoped, not
  persisted)`。

### 贡献系统提示词分段（路线 B）

```rhai
register_prompt_section("style", "Prefer small, reviewable diffs.");
```

分段按注册顺序追加到基础系统提示词（同 id 重复注册为原位替换）。分段在
mod 加载时注册、会话内稳定——对前缀缓存友好。限制：≤16 段、id 须匹配
`[a-zA-Z0-9_-]`、文本非空（违反即 mod **加载失败**）。任何 handler 经
`before-agent-start` 的整段替换仍优先于分段。reload 清除该 generation
的全部分段。

### 贡献技能（路线 B）

```rhai
register_skill(#{
    name: "commit-helper",
    description: "Write well-scoped commit messages",
    body: "# Steps\n1. Read the diff.\n2. Draft the message.",
    when_to_use: "the user asks for a commit",
});
```

注册技能是内存中的目录条目（磁盘上没有 `SKILL.md`）：会出现在系统提示词的
`## Skills` 块与 `/skills` 中，`load_skill` 按名解析——来源归因到
mod（`mod: <mod-id>`）而非文件路径。名字与文件系统目录冲突时文件系统获胜
（该注册被跳过并在 `/skills` 出警告）；名字已被**别的** mod 注册则本 mod
**加载失败**（错误点名持有者）。限制：≤16 个、名字须匹配
`[a-zA-Z0-9_-]`（1-64 字符）、description 与 body 非空。注册技能不带
`paths`，因此不参与条件（工作集）技能匹配；子代理会话不渲染技能目录。
reload 清除该 generation 的全部注册。

### 注册守卫（deny-only 工具策略）

```rhai
register_guard(|e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        "destructive command"
    }
});
```

守卫是 **deny-only** 的工具调用策略。闭包收到工具调用载荷（`|e|`——与
`tool-call` 钩子的载荷同形）；返回字符串即以该理由**拒绝调用**，返回其他
任何值（包括 `()`）则弃权。刻意没有 allow / transform 词汇：拒绝映射为
链短路的 block，因此注册顺序与其他 handler 都无法把一次拒绝翻回允许，
被拒结果带归因（`guard (mod: <mod-id>): <理由>`）。需要改写输入用
`on("tool-call", ...)`；需要策略底线用守卫。守卫脚本出错则弃权并打
`tracing` warn（与 handler 错误同款 fail-open 策略）。守卫在 tool-call
接缝处评估（审批之前——被拒调用根本不会派发，审批路径无从翻转）；仅覆盖
主回合调用（子代理注册表不绑 extension runner）；reload 清除该 generation
的全部守卫。

### 注册消息投影（会话日志折叠）

```rhai
register_message_projection("counts", #{ users: 0 }, |state, m| {
    if m.role == "user" { state.users = state.users + 1; }
    state
});
```

宿主把每条转录消息经 `fold(state, message)` 折叠进状态——追加式增量
折叠，整体替换（会话重载、压缩、`/edit` 回滚）则从 `init` 全量重折。
任何 natives 可用之处（钩子/工具/命令）都能用 `projection_state(key)`
读状态。状态从不以快照持久化：它始终是"当前转录的折叠"，因此跨会话
重载靠重建存活。与 `mod_state_get/set`（mod 自写的持久状态）的区别：
投影由宿主维护、从日志派生。

限制：`message` 为线格式消息 map（`role`、`content` 块）；fold 出错即
丢弃该投影直到下次 mod 加载（记日志，绝不杀回合）；mod 内 `key` 重复
即 mod **加载失败**；全部 mod 合计 ≤16 个投影；`/extension reload` 后
新 generation 的状态在下个回合开始时重建。

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
provider、header 值不是字符串、或 `base_url` 既非 `https://` 也非
回环 `http://` 端点（别名工厂会把用户的 api_key 和完整提示词发往
该 URL —— 明文 HTTP 远端被拒绝；Ollama、vLLM、LM Studio 等本地
网关仍被允许）—— 坏的 spec 到不了客户端。刻意不接受
`api_key`：秘密只存在于配置中，绝不进入脚本。

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

- **首次激活需确认**：新 mod 被发现 → 跳过加载 + REPL 被动提示待激活。激活有两条路：REPL 里的 `/mods activate <id>`，或审批 `manage_mods(action="activate")` 工具调用。激活记录持久化（`~/.codesmith/mods_state.toml`），同 id 重载免审批——**同意与入口文件的内容哈希绑定**：已激活 mod 的 `.rhai` 若随后变更（git pull、被自动批准的编辑），会回到待激活状态并要求重新确认。**为什么**：mod 是进程内代码且跨会话持久——提示注入可以在用户无感时植入常驻钩子，一次批准不能变成该 id 的常驻任意代码执行；首次确认（加内容变更后的重新确认）正是防这一点。
- **单 mod 装卸**：`activate` / `enable` 只编译并绑定这一个 mod；`disable` / `remove` 只驱逐它的编译产物，幸存 mod 从各自保留的已编译 AST 重新注册——不重编译其它 mod、不做整层重载。每次转换将 runner 世代号加一，注册变化仍在下一轮边界生效（`tools-change` 纪律不变）。`/mods reload`（及 `manage_mods(action="reload")`）仍是整层路径：重新发现、重新校验同意、全新编译。卸载只回收编译后的引擎与 AST——mod 的 KV 文件按设计持久保留（它是 mod 显式选择的状态存储）。
- **让智能体代写**：直接说"帮我写一个 mod，拦截 git push"——模型会经 `manage_mods` 工具写文件（`write`）并请求你审批激活（`activate`）。
- **mod 写入遵循 workspace trust，两个根都管**：未信任工作区不发现项目 mod，`manage_mods` 也拒绝在其中写 mod——项目根与全局 `~/.codesmith/mods` 一视同仁（全局写入会安装加载到所有工作区（包括其它未信任工作区）的代码，信任信号不能被 `global` 标志绕过）。先信任工作区，再写入。
- **已知边界**：mod 工具与 Rust 扩展工具同为"主 turn 独占"（子代理结构性不可见）；网络安装源（git clone 到 mods 目录）不在 MVP。

## 配置

`config.toml` 的 `[mods]` 节（默认 true）：

```toml
[mods]
enabled = true   # 总开关：发现、manage_mods 工具
```

`watch` 热重载文件 watcher 已在 0.9.0 移除；用
`manage_mods(action="reload")` 来加载改动。

## 实现索引

| 部件 | 位置 |
|---|---|
| `ModManifest` / `discover_mods` / 信任门 | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod`（Extension 实现、native 注册、事件映射） | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ModKvStore`（每 mod 持久 KV） | `crates/extensions/src/script/kv.rs` |
| `ModStateStore`（激活/禁用状态） | `crates/cli/src/mod_state.rs` |
| 共享操作层（manage_mods 工具背后的单一实现） | `crates/cli/src/mod_ops.rs` |
| 装配/门控接入（`populate` 返回 pending 报告） | `crates/cli/src/core/engine.rs` |
| `ManageModsTool`（模型可见） | `crates/cli/src/tools/mods.rs` |
| `/mods` REPL 命令 | `crates/cli/src/interactive/commands/mods_cmds.rs` |

## 延伸阅读

- [EXTENSIONS_cn.md](EXTENSIONS_cn.md) — Rust 扩展（dylib / 编译进二进制）：同一 `Extension` 契约的完整能力形态
- [HOOKS_cn.md](HOOKS_cn.md) — shell 命令形态的生命周期钩子（进程外，与本页进程内钩子互补）
