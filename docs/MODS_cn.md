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
| 事件钩子、工具守卫 | ✓ `on` / `on_event` / `register_guard` | — | ✓ | 按卸载/加载粒度为 mod；钩子错误 fail open |
| 模型可见工具 | ✓ `register_tool`（纯逻辑——无 fs/net 原生函数） | ✓（可脚本化的默认值） | ✓（完整 IO/异步） | 子代理可挂载 script/compiled-in 工具；dylib 来源的 Rust 工具保持仅主轮（5b 分层） |
| 既有工具的实现 | ✓ `replace_tool`（替换执行；`delegate()` 触达原实现） | — | ✓ | 除 description/schema 外的元数据沿用被替换者——替换无法放宽审批门槛；主轮与子代理注册表应用同一替换（见下） |
| 斜杠命令 | ✓ `register_command`（遮蔽内建）/ `disable_command`（退役内建） | — | ✓ | 层序 user > mod > builtin |
| Provider | 仅别名（`register_provider`） | — | ✓ 完整 `ProviderFactory` | 脚本无法实现 `LlmClient`（无异步/网络） |
| 提示词分段、技能 | ✓ `register_prompt_section` / `register_skill` | ✓（默认值） | ✓ | 遵守前缀缓存纪律 |
| 策略（宿主机制旋钮） | ✓ `register_policy`（last-wins） | ✓——默认值本身就是内建 mod | ✓ | 数据而非代码；`/mods list` 显示解析结果 |
| turn 循环（所有 agent） | ✓ `register_loop`（驱动，经 `rhai` executor） | — | ✓ `register_around_turn`（包裹） | 内核组合——*选择*：配置 `turn_executor` 选中 crate 内编译的 `TurnExecutorRegistry` 条目（`builtin`，或 `"rhai"` 运行某个 mod 的 loop 脚本），主 turn 与每个进程内子代理 turn 一视同仁；*组合*：Rust 扩展以 通用 Agent `next()` 包裹 run（每个 turn——子代理在其 agent 类型作用域上组合该链）；脚本 loop 只拥有循环的**形状**——其能力调用（`llm_request`、`run_tools`）执行宿主的步骤服务（见"编写 loop 脚本"） |
| 压缩策略 | ✓ `register_compaction_strategy`（trigger + plan 纯决策） | ✓（opt-in 移植经 `[compaction] strategy` 选择） | — | mod 拥有*决策*（何时压缩、保留多少、摘要指令）；宿主拥有*效果*与免疫区（配对不变量、系统提示前缀、append-only 存储、摘要 LLM 调用、断路器）。选择：config `[compaction] strategy = "builtin" \| "<name>"`（见"编写压缩策略"） |
| 压缩清扫规则（micro-compact 资格判定） | ✓ `register_sweep_rule`（编译的 `eligible` 闭包 + 替换参数） | ✓（opt-in 移植；装载规则即切换） | — | 编译闭包决定一次清扫可清哪些旧工具结果（其存在还改变清扫节奏）；宿主拥有清扫原语（替换执行、字节计量、幂等、配对）与时间/字节触发基线 |
| `BASE_PROMPT`、子代理工具注册表 | — | — | — | 契约声明的核心（唯一基础提示词、子代理注册表仅内建） |

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
- 现在**改 `mod.rhai`、保存，然后重载**——`/mods reload` 会重新执行发现流程，同 id 免审批。`/mods status` 确认哪些 mod 已加载及运行时代数（`/mods list` 显示各 mod 的激活状态与已解析策略层；启动提示也会列出已发现但未激活的 mod）。（自动文件 watcher 及其 `[mods] watch` 开关已在 0.10.0 移除，重载改为显式触发。）

到此你已经用过全部四种注册面：事件钩子、工具、命令、KV。`/mods` 的完整命令集为 `list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`；模型侧 `manage_mods` 工具与之对应，动作为 `list` / `write` / `activate` / `disable` / `enable` / `remove` / `reload`，另有会话实验线 `session_write` / `session_remove`（见下文）。

## 速查

### 事件钩子

`on("<event>", |e, ctx| { ... })`——`e` 是事件 payload map（都带 `kind` 字段），`ctx` 是 `#{cwd, mode, idle, generation, agent}`（`mode` 在交互终端 REPL 下为 `"tui"`——沿用已移除前端的名字以保兼容——其余为 `"rpc"` / `"json"` / `"print"`），**ctx 可省略**（写 `|e|` 即可）。`agent` 是本次分发的 agent 身份——`#{main, type}`，主 turn 下 `main` 为 `true`、`type` 为 `()`，子代理 turn 下 `type` 为其类型——回调由此区分子代理 turn 与主 turn（`if !ctx.agent.main { return (); }` 即标准的"仅主 turn"守卫）。事件名是 `ExtensionEventKind` 的 kebab-case 全量 32 种：

| 事件 | payload 字段 |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`、`inject_message`（`()` 表示未设置） |
| `before-provider-request` | `messages`（JSON 值） |
| `after-provider-response` | `response`（JSON 值——组装后的 `MessageResponse`；流式路径下 `id` 为空） |
| `before-provider-headers` | `headers`（JSON 对象；含凭据性质的值已掩码） |
| `provider-request-error` | `failure`（`#{category, message}`）、`attempt`、`plan`（`#{action: "retry" \| "give-up", delay_ms}`） |
| `tool-call` | `id`、`name`、`input`（JSON 值） |
| `tool-result` | `id`、`name`、`content`、`success`、`is_error`、`canonical`（JSON 值，工具未产出时为 `()`） |
| `turn-start` / `turn-end` | `turn_id`；`turn-end` 另有 `reason` 与活动摘要 `steps`（本轮开始的循环迭代数）、`tool_calls`、`usage`（`#{input_tokens, output_tokens}`） |
| `turn-stopping` | `reason`（轮即将结束的原因——唯一 emit 位点处为 `no-tool-calls`）；在轮因无工具调用而结束前发出；`cancel("提示")` 即 steer——提示追加为 user 消息并续轮（每轮上限 3 次） |
| `assistant-stream` | `text`（单个增量块；随文本 delta 逐次触发，仅 emit） |
| `tool-execution-update` | `id`、`name`、`message` |
| `tools-change` | `added`、`removed`（工具名数组） |
| `workflow-start` | （仅 `kind`）——一次 workflow 运行开始（预算被拒的运行不触发） |
| `workflow-log` | `message`（workflow 运行中一条有界的 `log(...)` 日志） |
| `workflow-agent-start` | `objective`（一个 workflow 子级开始派生） |
| `workflow-agent-end` | `status`、`steps`（一个 workflow 子级结束） |
| `workflow-end` | `status`、`error`（失败时的错误类别）、`agent_calls`（workflow 运行的终态结果；不携带返回值——转录中的工具结果即记录） |
| `project-trust` / `session-start` | `reason` |
| `result-verified` | `verdict`（`verified-pass` \| `verified-fail` \| `verify-error` \| `unsubstantiated`）、`claim`、`command`（无可重跑命令时为 `()`）、`exit_code`（未运行时为 `()`）——结果声明验证器的判定，由 turn 结束后的后台重放触发，落在两个 turn 之间 |
| `session-before-compact` | `current_tokens`（整个转录的估算）、`threshold`（配置的 token 阈值）、`pinned_hints`（字符串数组——transform 可写、链上可见、v1 宿主未消费）——触发输入的参考读数；实际决策对比的是按计划形状选取的子集与按 pin 调整后的有效阈值，因此否决时衡量的是这两个输入，而非字面操作数 |
| `session-compact` | `summary`（压缩摘要的头部，截断作预览——完整提示词已记录供宿主 run 后合并）、`kept_messages`（压缩替换与回注应用后的转录消息数） |
| `agent-start` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` | （仅 `kind`） |

`tool-execution-update` 由 shell 工具自己的轮询循环在命令持续运行时发射（`exec_shell`
前台与 `exec_shell_wait`）：至多每秒一次、且仅当有新输出。`id` 是 shell 任务 id（工具
不知道自己的 wire tool-call id——按 `name` + 时序关联）；`message` 是自上一检查点以来的
输出（尾部截至 4 KiB；管理器给出截断摘录时取其尾部）。仅 emit、仅主回合（子代理 turn
的回调不接工具进度汇）；沙箱后端与交互式运行不报检查点。同一检查点也以 `ToolCallProgress`
到达 UI。`resources-discover` 已在 0.11.0 移除（从未有发射方，也没有消费者）。
`session-before-fork` 在每次 spawn 前触发——spawn 工具族（`agent_open` / `agent_spawn` /
`agent_run`）在任意深度（runner 随子代理运行时传递，
故孙代 fork 同样可否决）以及引擎内部 spawn 路径——在 fork 会话状态前触发；`cancel()` 否决
该次 fork。自 5b 起，runner 在子代理侧不再只是否决载体：子代理注册表会挂载该子代理类型
的扩展工具视图（仅 script/compiled-in 来源——因此 mod 的 guard 同样把关子代理工具
调用），而每个子代理 turn——与主 turn 同一个循环——在其 agent 类型作用域分发自己的
生命周期：`before-agent-start`、`agent-start` / `agent-end`、`turn-start` / `turn-end`、
`turn-stopping`、provider 事件、`assistant-stream`、`tool-call` / `tool-result` 与 `tool-execution-start` /
`-end`（仅 script/compiled-in 来源的处理器；dylib 处理器永远看不到子代理），其后续输入
（`agent_send_input`）以 steer 形态经过 `input`。会话级事件（`session-*`、`agent-settled`、
`tools-change`）仍只属主 turn。

`tool-result` 载荷携带 canonical/rendered 分离：`content` 是模型可见的渲染文本；`canonical` 是同一结果的结构化机器可读值（`ToolResult::json` 的值，或工具经 `with_canonical` 附加的值）。请读 `canonical` 而不是重新解析 `content`——散文式渲染的结果（如 `todo_add` 在 JSON 前拼了一句提示）根本无法解析。canonical 仅存活于当前进程：转录持久化的是渲染后的 `content`。

`tools-change` 在回合边界触发：当编译后的模型可见目录相对上一主回合发生变化（模式切换、mod 重载、选择变更）。首回合只建立基线、不触发；sub-agent 工具集不会触发；没有读取当前目录的 API——事件只携带差量。`/tools` 按来源（builtin / plugin / extension / mcp）分组展示当前基线。

分发模式是每个事件契约的一部分（`ExtensionEventKind::dispatch_mode`，由契约测试锁定）
—— 三态命名对齐通用 Agent 模型：**waterfall**、**serial**、**emit**：

- **waterfall**：`input`、`before-agent-start`、`before-provider-request`、`tool-result` —— transform 逐个折叠、后续 handler 可见、终值作用于宿主操作
- **waterfall + deny**：`tool-call` —— 改写调用 `input`（改写后的输入才是审批与实际执行的输入，也是入档输入），或 `block(reason)` 拒绝（单调——后续 handler 不可翻转）；`provider-request-error` —— 改写重试 `plan`，或 `block(reason)` 放弃
- **serial**：`session-before-switch`、`session-before-fork`、`session-before-compact`、`turn-stopping`（倒置：`cancel(reason)` 让轮继续——提示即 steer）
- **emit**（outcomes 仅供参考）：其余全部，含 `assistant-stream`（随文本 delta 逐次触发——handler 须轻量）与 `tools-change`（回合分发收口的目录差量）

#### `on_event` —— 通用 Agent 对齐的监听/否决形态

`on_event("<event>", |e, ctx| { ... })` 的回调形态与 `on` 完全相同
（同样的 payload、同样可省略的 `ctx`、同样的控制值），但只接受 **emit**
与 **serial** 事件——可直迁 通用 Agent 的子集。waterfall 事件（`input`、
`before-agent-start`、`before-provider-request`、`tool-result`、
`tool-call`、`provider-request-error`）会让 mod **加载失败**，错误点名
该事件：它的 transform 链属于 `on()`，拒绝底线属于 `register_guard`。
emit 事件下一切返回值仅具参考性；serial 事件下 `cancel(reason)` 即否决。
只监听或只否决的 mod 用 `on_event`——这样的 mod 与 通用 Agent 的 emit/serial
模型一一对应；需要完整 CodeSmith 表面（含 waterfall transform——刻意
超出 通用 Agent 脚本面的超集）时用 `on()`。

作用域：`on_event` 注册是 global 作用域、script 来源——global 注册覆盖
所有分发作用域，因此 `on_event` 回调既观察到主 turn 的 emit/serial 事件，
也观察到每个子代理 turn 的（`turn-start`、`turn-end`、`agent-start`、
`agent-end`、`turn-stopping`、provider emit、`assistant-stream`、`tool-execution-*`）。
分发身份随 `ctx.agent`（`#{main, type}`）携带：回调以 `ctx.agent.main` 区分
子代理的 turn 与主 turn——但无法区分同一类型的两个子代理实例（`type` 是
作用域单位，没有按实例的 id）。没有 `on_event_scoped`——脚本看不到
注册作用域（编译内置扩展可注册 `on_variant_scoped`）。around-execute 与
`ask` 在任何通道都没有脚本形态（异步 / UI 决策通道——见
[EXTENSIONS_cn.md](EXTENSIONS_cn.md)；不存在 `ask()` 控制值，脚本永远
产生不了 `Ask` 结果）。

### 钩子返回值 → HandlerOutcome

| 脚本返回 | HandlerOutcome | 生效 seam |
|---|---|---|
| `proceed()` 或 `()` | `Continue` | 全部 |
| `block(reason)` | `Block`（越权 seam 忽略） | `tool-call`、`provider-request-error` |
| `cancel(reason)` | `Cancel`（越权 seam 忽略） | `session-before-*`、`turn-stopping`（steer 让轮继续） |
| `transform(#{...})` | `Transform`（合并可变字段后继续链） | 见下 |

Transform 可变字段（一个 handler 的改写对后续 handler 立即可见）：

- `input`：`text`——每条提交的消息以及 turn 运行中输入的每条消息（steer）的文本；改写后的文本即被记录与发送的内容
- `before-agent-start`：`system_prompt`、`inject_message`（字符串=设置，`()`=清除，缺省=保持）
- `before-provider-request`：`messages`
- `tool-result`：`content`、`success`、`is_error`、`canonical`（JSON 值——结果的机器可读值；仅改写 `content` 不动它，可显式替换）
- `provider-request-error`：`retry`（布尔——`false` 放弃）、`delay_ms`（整数；缺省时保持当前 plan 的值；强制重试在引擎侧有上限）
- `session-before-compact`：`pinned_hints`（字符串数组=设置，`()`=清除，缺省=保持）——v1 仅链上可见：宿主尚未消费文本提示（压缩计划器按消息序号 pin），transform 写入的是链上后续 handler 读到的值

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
| `replace_tool(spec, callback)` | 接管一个已注册工具的执行（见下） |
| `delegate(input?)` | 替换回调的控制值：运行原实现——`()` 原样透传入参，map 则改写后传入 |
| `on_event(kind, callback)` | 注册 通用 Agent 对齐的钩子——仅 emit/serial 事件（waterfall 事件导致加载失败；见上） |
| `register_message_projection(key, init, fold)` | 注册由宿主维护的会话日志折叠（见下） |
| `projection_state(key)` | 读取本 mod 的投影状态（钩子/工具内可用） |
| `register_policy(kind, config)` | 覆盖宿主机制策略（见下） |
| `disable_command(name)` | 停用一个内建斜杠命令（见下） |

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
  `cleared_placeholder`、`keep_head_chars`、`keep_tail_chars`（编译
  内建清扫规则的替换参数——清除后围绕占位符保留的头/尾字符；omp 式
  逐轮淘汰现为编译清扫规则 `register_sweep_rule`，不再是旋钮）
  —— 压缩引擎本体留在 Rust；摘要提示词由多个会话
  来源合并而成、并非单一模板，故不进此接缝）、`large-output-route`
  （`threshold_tokens`、`per_tool` —— 路由决策；utility LLM 综合调用留在
  Rust。显式 `[workshop]` TOML（`large_output_threshold_tokens` /
  `per_tool_thresholds`）位于该种类之上：config 值优先，TOML 未设时由
  脚本层提供默认值）、`working-set`（`recent_read_file_limit`、
  `recent_read_file_snippet_chars` —— 会话保留的最近读取文件窗口，
  使被压缩掉的读取仍可用；留存机制留在 Rust）、
  `knowledge-on-demand`（`max_session_bytes`、`max_entrypoint_lines`、
  `max_entrypoint_bytes`、`enabled_types` —— 记忆呈现的会话字节预算、
  MEMORY.md 索引预算、可呈现的记忆类型子集；扫描、相关性旁路查询与
  截断留在 Rust。预算单位是字节而非 token。刻意不设相关性*阈值* ——
  旁路查询挑选文件名，不打分）、`tool-output-gate`（`read_max_lines`、
  `read_max_bytes`、`read_hard_max_lines`、`grep_max_results` —— 读/搜
  面的 omp 式统一输出档位；门控本体——UTF-8 安全切片、可执行的续读
  提示——留在 Rust）。
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
  `builtin-large-output-route`、`builtin-working-set`、
  `builtin-knowledge-on-demand` —— 源码在
  `crates/extensions/src/script/builtin/`）。
  它们始终装载（`[mods] enabled = false` 也只禁用户 mod）、跳过同意/哈希
  门（随二进制发布）、使用临时 KV，且不出现在 `/mods` 的 mod 列表（不是
  可发现 id —— 覆盖方式是注册同种类策略，而非遮蔽 id；其 id 仅作为
  `list` 已解析策略节的来源出现）。
- **opt-in 内置 mod**：改变行为的内置 mod（工具替换、loop、压缩策略）
  同样随二进制发布，但仅当 `[mods] builtin = ["<id>", …]` 列名时才装载
  ——该配置键是一切"不应常驻"的移植实现的统一"按配置加载"入口。opt-in
  id 不带 `builtin-` 前缀（id 即用户面对的选择键，与选择它的配置键共享
  ——如 `[compaction] strategy`）。列出的 id 匹配不到任何 opt-in 内置
  mod 会让启动审计大声失败；未被选中的 opt-in mod 编译进二进制但从不
  运行。reload 重放同一选择集。当前 opt-in id：
  - `compact-pi`、`compact-omp`、`compact-minimax`、`compact-dsh`——
    自五 agent 对比文章移植的压缩策略（取值与出处见各 mod 头部注释；
    经 `[compaction] strategy` 选择，见"编写压缩策略"）。
  - `grep-guard`——grep 输入整形（空 pattern 拒绝并附纠正动作；
    include glob 逗号拆分，花括号组保留）。
  - `edit-input-guard`——`edit_file` 输入守卫：search/replace 相同与
    替换比例失衡时拒绝并附纠正下一步。
  - `reject-templates`——破坏性 shell 模式（`rm -rf /`、force-push、
    `reset --hard`）的 deny-only 守卫，拒绝附纠正模板。
  - `runaway-guard`——失控检测（minimax `exact_action_repeat` 精确重复
    信号，提醒强度）：连续第三次完全相同的工具调用被拒绝，附循环
    识别与出路。
  - `goal-loop`——dsh 目标状态机的 loop 脚本形态：phase
    （active/complete/blocked/paused）+ mod KV 修订计数、每轮
    `compact_now()`、连败熔断（需 `turn_executor = "rhai"` +
    `[rhai_loop] mod = "goal-loop"`）。
  - `verify-loop`——omp diagnosticsOnWrite 的 loop 脚本形态：编辑批次
    后立即跑一批 `diagnostics`（与 `goal-loop` 同三键启用）。
  - `elide-uneventful`——omp 逐轮淘汰的编译清扫规则形态：已完结轮次的
    无事件结果凭存在即折叠；活动轮次逐字保留（见"清扫规则"）。

### 停用一个内建斜杠命令

```rhai
disable_command("model");
```

停用会隐藏**内建**命令的表项——分派、`/help` 与 Tab 补全都跳过它
——这样 mod 无需提供替代实现即可退役一个内建命令。同名的用户命令与
mod 注册命令仍然可用（user > mod > builtin 层序不变）。宿主会按其内建
表解析名称（规范名与别名均可）；未知名称警告并空操作（target
`codesmith_mods`——脚本缺陷走警告姿态，与坏策略旋钮一致）。停用随该
generation 的其他绑定注册一并清除：`/mods reload`、`disable`、
`remove` 会从幸存 mod 重新推导。配置侧的等价物是 config.toml 的
`[commands] disabled = [...]`（那里的未知名称会让 REPL 启动大声失败
——用户手写的配置走大声姿态，脚本缺陷走警告）。

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
  编译形态，无重编译）。会话恢复不重建。其策略按引擎隔离：本引擎的每个
  turn——包括子代理的机制——读同一 store，同进程的其他引擎各读各的；单引
  擎的 CLI 把自己的 store 注册为进程默认，因此引擎外的工具函数也能读到；
  其工具/命令不进入子代理注册表（设计上仅内建）；fork 否决覆盖所有代际。
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
接缝处评估（审批之前——被拒调用根本不会派发，审批路径无从翻转），同样
把关子代理工具调用（子代理的 turn 按其 agent 类型作用域分发
`tool-call`，script 来源的处理器一并覆盖）；reload 清除该 generation
的全部守卫。

### 替换内建工具（通用 Agent 模式 d）

```rhai
replace_tool(#{
    name: "grep_files",              // 要接管的已注册工具（必填）
    // description: "受限 grep",      // 可选的模型可见覆盖
    // schema: #{ type: "object", ... },  // 可选覆盖——必须是 object 根
}, |input, ctx| {
    if input.query.contains("secret") { return err("不许搜敏感词"); }
    delegate(#{ query: input.query, max_results: 50 })   // 改写后运行原实现
});
```

`register_tool` 是*新增*工具；`replace_tool` 替换一个**已注册工具的执行**。
回调收到 `(input, ctx)`（与 `register_tool` 回调同形——ctx 可省略），逐次
调用做出决定：

| 回调返回 | 效果 |
|---|---|
| `ok(value)` / `err(msg)` / 裸值 | 以该结果替代——原实现不再运行 |
| `delegate()` | 以（经 waterfall 之后的）原入参运行原实现 |
| `delegate(#{...})` | 以改写入参运行原实现（先对其做原实现的校验） |
| 其它控制值（`block` 等） | 返回错误结果，提示模型应使用 `ok`/`err`/`delegate` |

性质与限制：

- **安全元数据不容替换更改。**能力集、审批阶梯、破坏性判定、路径问题、
  并发约束全部沿用被替换者——替换永远无法放宽审批门槛，只读表面（explore
  子代理）也依旧按被替换者的能力收窄。仅 `description` 与 `schema` 可覆盖
  （目录随之变化；被替换的工具始终立即可见，不会 defer 在 tool search 之后）。
- **组合次序**：内建 → 配置 `[tools]` override → mod 替换，后到的 mod 包住
  先到的——`delegate()` 触达替换生效时刻的被替换者，因此链可组合且永不
  成环。两个 mod 替换同一工具会告警（点名双方）并成链；**同一 mod** 内对
  一个名字同时 `register_tool` + `replace_tool` 会导致加载失败。
- **目标缺失是告警而非错误**——表面随模式变化（Plan 模式裁掉写工具；
  explore 子代理只读），替换在那里直接跳过。主轮的 MCP 工具走目录合并非
  注册表常驻，v1 无法替换。
- **作用范围**：替换是全局的——每个 turn（主轮与子代理一视同仁）在注册表
  组装时解析，下一 turn 生效；reload/停用同样于下一 turn 恢复内建。
  `tool-call` 改写与 `tool-result` 变换 waterfall 照旧围绕被替换工具运行
  （dispatch 层接缝，不变），组合模式是：`replace_tool` 管执行策略与改写、
  `on("tool-result", ...)` 管后处理。
- 替换回调同步执行脚本（受 mod engine 操作数上限约束）；`delegate` 在
  turn 的正常取消机制下执行。

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

一个值得先知道的 Rhai 语义（否则 handler 会静默 fail-open）：**命名的顶层 `fn` 看不到顶层 `const`**（每次调用的调用状态是全新的），而事件回调可以（闭包会捕获）。命名 fn 需要的一切都走参数传入——写 `fn record(lesson, max)` 并以 `record(lesson, MAX_REFLECTIONS)` 调用，不要写 `fn record(lesson)` 在体内读 `MAX_REFLECTIONS`。

第二个，写给致密的 loop 脚本：**Rhai 按函数体限制表达式深度（默认 32）**——很长的双重嵌套块（`for` 套 `for`，每条语句又是 map 字面量调用）会在编译时报 "Expression exceeds maximum complexity"。把循环体提升为命名函数（`fn search_round(frontier, …)`）即可——每个函数体重新获得完整的解析预算。

## 示例 mod

仓库在 [`examples/mods/`](../examples/mods/) 下内置两个行为插件范例——把任一目录复制进 `~/.codesmith/mods/`（或工作区 `.codesmith/mods/`）后用 `/mods activate <id>` 激活。两者都有加载并驱动行为的测试（`crates/extensions`），不会静默腐烂。

- **`reflexion`**——Shinn et al.（2023）的 mod 化：失败的 `tool-result` 与 `result-verified` 的 `verified-fail` 判定累积进反思账本（mod 的 KV），下一次尝试开始时以一条注入的 user 消息携带（`before-agent-start` 瀑布），首个干净 turn 将其清空。仅主 turn（`ctx.agent.main` 守卫）。
- **`voyager-capture`**——Wang et al.（2023）的采集半边：干净的主 turn 动作序列（≥ 3 次成功的动作类调用、零失败）被蒸馏为技能存入 mod 的 KV；每次装载经 `register_skill` 重新注册累积的库，采集到的流程在 `/mods reload` 后回归。
- **`react-min`**——loop 范例：builtin 的 ReAct 形状写成 ~20 行脚本。激活它并设 `turn_executor = "rhai"`，turn 即运行该脚本而非编译循环——流式、审批、事件纪律全部一致，因为每次 `llm_request()` / `run_tools()` 调用执行的都是宿主的步骤服务。引擎的等价性测试以 mock provider 驱动它。
- **`lats`**——带**扇出**的 loop 范例：best-first 树搜索——`llm_request(#{prompt})` 扩展与评分、`spawn_child` 候选模拟、beam 回溯。子代理也运行会话的 loop，因此脚本按 `spawn_depth()` 分角色（0 为搜索者，之上为评估者）。旋钮走 `lats-policy` 策略种类（`width`/`depth`/`beam`，经 `loop_policy_int` 读取）。教程见"编写 loop 脚本"。

## 编写 loop 脚本

`register_loop("main")` 声明入口；选中 `turn_executor = "rhai"` 时，
`fn main() { ... }` 每个turn 在专用线程上运行一次。能力面是 loop 引擎注册的
一组全局函数：

| 调用 | 返回 / 行为 |
|---|---|
| `llm_request()` | `#{text, tool_uses: [#{id, name, input}], stop_reason, usage: #{input_tokens, output_tokens}, interrupted}`——一个 LLM 轮：请求组装（三区、prefix-cache 观测）、`before-provider-headers` / `before-provider-request` 瀑布、带透明重试的流式、转录持久化。排队的 steer 在请求前排入转录（builtin 循环的步首行为）。 |
| `llm_request(#{prompt})` | 同一轮，但先把 `prompt` 作为 user 消息排入转录（持久化——模型可见即有记录）。脚本借此在一份共享转录上按用途区分轮次——扩展、评分、作答。 |
| `run_tools(calls)` | `#{halt, plan_approved, errors}`——一个工具批：审批、沙箱、guard、`tool-call` / `tool-result` 瀑布与结果持久化。`halt != ()` 表示 loop guard 触发——应结束 turn。 |
| `spawn_child(spec)` | `handle`（int）——从 `#{prompt, type?, allowed_tools?}` 起一个进程内子代理 turn，走宿主 spawn 路径同一条 child-turn 接缝（`run_child_turn`：子代理运行会话的 loop、触发 `SessionBeforeFork` 否决、继承父工具面——`allowed_tools` 命名子集时收窄，未知名响亮失败）。上限越界响亮报错：`[runtime] max_spawn_depth`（v1 扇出**仅一层**——子代理自己的 `spawn_child` 会被拒绝）与 `max_subagents`（每 turn 存活子代理数）。 |
| `eval_child(handle, timeout_ms)` | `#{status, final_text, steps}`——等待一个已 spawn 的子代理（`status` 为 `completed`/`cancelled`/`failed`；未完成时 `final_text` 为 `()`）。超时中止该子代理并报错——被放弃的分支不得滞留。 |
| `spawn_depth()` | 本 turn 的深度（`0` = 主 turn）——子代理运行同一脚本，搜索/评估的角色切换就靠它。 |
| `loop_policy_int(field, default)` | 读一个 `lats-policy` 旋钮（`width`/`depth`/`beam`）——按后写者胜解析策略，脚本的编译 `default` 兜底；用户 mod 的 `register_policy("lats-policy", …)` 无需重编译即可调参运行中的 loop。 |
| `compact_now()` | `#{ran, messages}`——按需运行宿主的压缩步骤服务（micro-compact、触发门——含已选的 `[compaction] strategy`——LLM 摘要）。`ran == false` 是干净的空操作（未过阈值/被禁/断路器开启），不是错误。与 builtin 循环不同，没有自动的请求前预检：脚本在自己的形状需要处调用，典型是每次 `llm_request` 之前。 |
| `cancelled()` | turn 的取消令牌触发后返回 `true`——长循环里要检查。 |
| `note(text)` | 把脚本决策记录到会话日志路径（现场呈现与持久记录一致）。 |

错误姿态**响亮失败**（与行为 hook 相反）：脚本错误使 turn 失败。
config.toml 的 `[rhai_loop]` 固定 mod（`mod = "react-min"`）并限定单个
turn 的脚本执行时长（`timeout_secs`，默认 3600）。脚本 loop 不继承
builtin 循环的 LSP 冲洗、容量门与 `turn-stopping`
steer 接缝——移植需要这些的循环前，先看 `rhai_executor` 模块的已知限制。

### 教程：LATS——一个搜索型 loop

[`examples/mods/lats`](../examples/mods/lats/) 把 best-first 树搜索
（Zhou et al., 2023）写成一个 loop 脚本——补上 react-min 缺的形状：
脚本*主导*下一问什么，而不是对一条流被动反应。四个动作覆盖整个模式：

1. **按用途区分轮次。** 每次 `llm_request(#{prompt})` 先把 prompt 追加进
   转录再发起该轮，于是扩展（"提出 W 个细化方向"）、评分与最终作答是
   同一份共享历史上的独立模型调用——且全部持久化（模型可见即有记录）。
2. **扇出。** `spawn_child(#{prompt, type})` 经宿主使用的同一条
   child-turn 接缝起一个子代理 turn；`eval_child(handle, timeout_ms)`
   等待并返回 `#{status, final_text, steps}`。子代理运行*会话的* loop
   ——同一个脚本——所以有：
3. **角色切换。** `if spawn_depth() > 0 { llm_request(); return; }` 让
   子代理成为评估者：对它的任务跑一轮即止。没有这个分支，搜索型脚本
   会在自己的子代理里继续搜索（它们的 spawn 在 v1 单层上限下也会被
   拒绝）。
4. **回溯。** frontier 是纯脚本状态——评分后的子节点连同父节点，每轮
   按 best-first 剪到 `beam`。转录无需回退；搜索叙事本身就是日志。

搜索旋钮（`width`/`depth`/`beam`）走 `lats-policy` 策略种类：内置层
发布缺省，`loop_policy_int("width", 3)` 以脚本的编译兜底读解析值，用户
mod 的 `register_policy("lats-policy", …)` 即可调参运行中的会话——数据
而非代码，经 `/mods list` 的已解析策略节自证。

## 编写压缩策略

`register_compaction_strategy(name, spec)` 让 mod 拥有压缩的**决策**；
config `[compaction] strategy = "<name>"` 选择它（`"builtin"`（默认）
保持编译内建算法）。接缝切分干净：

- **脚本可拥有**（对宿主构造快照的纯决策）：*何时*压缩（`trigger`
  闭包）与*保留多少* + 摘要指令（`plan` 闭包）。
- **宿主拥有**（免疫区，策略不可触碰）：tool-call/tool-result 配对、
  系统提示前缀、append-only 存储、摘要 LLM 调用及其重试、断路器、
  快速回填检测、最小可摘要条数下限。

```rhai
register_compaction_strategy("compact-pi", #{
    // snapshot: #{ total_tokens, window, threshold, messages,
    //             rounds: [#{ start, len, tokens, has_tool_use, has_media }] }
    trigger: |snap| snap.total_tokens > snap.window - 16384,
    plan: |snap| #{
        keep_recent_tokens: 20000,        // 或 keep_recent_rounds / keep_recent_messages
        summary_instructions: "…",        // 可选，追加到摘要提示
        summary_max_tokens: 16384,        // 可选，覆盖输出上限（256..=65536）
    },
});
```

移植其他 agent 的策略前值得了解的语义：

- **快照形状。** token 数由宿主预计算；round 按 assistant 起始分组
  （round 边界天然是配对安全切点）。`window` 是解析后的上下文窗口，
  `threshold` 是配置的 token 阈值。
- **宿主门仍在。** `enabled`、自动压缩下限、断路器与最小消息数在策略
  之前/周围检查——策略收紧时机，不能解除安全。
- **plan 解释有校验。** `keep_recent_rounds` / `keep_recent_tokens` /
  `keep_recent_messages` 三选一（> 0）；被拒的计划大声记日志并按内建
  边界执行本次压缩（trigger 已判定需要压缩）。
- **round 对齐保留。** `keep_recent_tokens` 从尾部按*整 round* 反向累加
  至预算，随后无论如何都会跑 `enforce_tool_call_pairs`——即便策略给出
  切开配对的边界，免疫区也成立。
- **选择与失败姿态。** 名称在压缩时对已装载 mod 解析（策略集是会话
  状态——mod 围绕配置装载/重载，因此不同于 `turn_executor`，无法在
  配置装载时校验）。未知名是每次尝试的大声错误并回落内建：压缩是安全
  机制，配置笔误不能卡死它。解析按装载序后写者胜——用户 mod 复用同名
  即覆盖内置策略。
- **范围（v1）。** 策略治理自动压缩路径（`run_compaction`）。容量干预
  与恢复压缩保持内建阈值；摘要请求*形状*（cache 对齐 vs 格式化）归
  宿主。已知限制写在接缝旁
  （`crates/agent-runtime/src/compaction/strategy.rs`）。
- **单一调用路径。** 内建算法本身也是一个决策单元
  （`CompiledBuiltinDecisions` —— 编译公式，等价性由测试钉死）；
  `"builtin"` / 未知名 / 无 mod 系统都解析到它。引擎的 `run_compaction`
  从不按"拿到哪个单元"分支——解析出一个 `CompactionDecisions`，问它
  同样两个问题。切换行为 = 换编译单元，不是切代码路径。

## 清扫规则（micro-compaction 的资格判定）

`register_sweep_rule(name, spec)` 让"先扫后搬"层的资格判定成为
**编译代码**：`eligible` 闭包决定一次清扫可清哪些旧工具结果，且规则的
存在本身改变清扫节奏（已完结轮次的结果被认领即触发——Rust 里没有任何
开关旋钮）。宿主拥有清扫原语：替换执行、字节计量、幂等、配对，以及
时间/字节触发基线。

```rhai
register_sweep_rule("uneventful", #{
    // view: #{ tool, is_error, has_media, content_chars,
    //         content_head（小写、≤512 字符）, in_live_round }
    eligible: |r| !r.in_live_round && !r.is_error && !r.has_media,
    placeholder: "[Uneventful result elided]",
    keep_head_chars: 0,   // 可选，dsh 式占位符前保留的头
    keep_tail_chars: 0,   // 可选，同理保留的尾
});
```

分派：已装载规则按装载序先认领；编译内建规则（可清工具清单 +
`compact-policy` 替换参数）恒在最后应答——无规则时行为与接缝落地前
逐字节一致。规则按次解析——`/mods reload` 下一次清扫即生效。opt-in
`elide-uneventful` 内置 mod 即 omp 移植（见上方 opt-in 清单）。

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
- **已知边界**：mod（script）工具与编译内置的 Rust 扩展工具自 5b 起也可挂载进子代理；dylib 来源的 Rust 扩展工具保持仅主轮（卸载安全）。网络安装源（git clone 到 mods 目录）不在 MVP。

## 配置

`config.toml` 的 `[mods]` 节（默认 true）：

```toml
[mods]
enabled = true   # 总开关：发现、manage_mods 工具
```

`watch` 热重载文件 watcher 已在 0.10.0 移除；用
`manage_mods(action="reload")` 来加载改动。

## 实现索引

| 部件 | 位置 |
|---|---|
| `ModManifest` / `discover_mods` / 信任门 | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod`（Extension 实现、native 注册、事件映射） | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptToolReplacement` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ToolReplacement` 接缝 + `ReplacementOutcome` | `crates/agent/src/extension.rs` |
| `ReplacementToolSpec` + 注册表应用（主轮 / 子代理） | `crates/agent-runtime/src/tools/replacement.rs` |
| `ModKvStore`（每 mod 持久 KV） | `crates/extensions/src/script/kv.rs` |
| `ModStateStore`（激活/禁用状态） | `crates/cli/src/mod_state.rs` |
| 共享操作层（manage_mods 工具背后的单一实现） | `crates/cli/src/mod_ops.rs` |
| 装配/门控接入（`populate` 返回 pending 报告） | `crates/cli/src/core/engine.rs` |
| `ManageModsTool`（模型可见） | `crates/cli/src/tools/mods.rs` |
| `/mods` REPL 命令 | `crates/cli/src/interactive/commands/mods_cmds.rs` |

## 延伸阅读

- [EXTENSIONS_cn.md](EXTENSIONS_cn.md) — Rust 扩展（dylib / 编译进二进制）：同一 `Extension` 契约的完整能力形态
- [HOOKS_cn.md](HOOKS_cn.md) — shell 命令形态的生命周期钩子（进程外，与本页进程内钩子互补）
