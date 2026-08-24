# 容量控制器

`codesmith-tui` 内置一个可选启用的容量感知上下文控制器。在默认的 V4
路径中它处于禁用状态，因为它的主动干预会改写实时 prompt 并破坏前缀缓存
亲和性。除非显式设置 `capacity.enabled = true`，请把它当作遥测或实验性
护栏看待。

## 策略概览

每个检查点计算：

- `H_hat`（运行时压力代理）
- `C_hat`（模型容量先验）
- `slack = C_hat - H_hat`
- 基于最近 `N=8` 次观测的动态 slack 剖面

### 运行时压力代理（`H_hat`）

- `action_complexity_bits = log2(1 + action_count_this_turn)`
- `tool_complexity_bits = log2(1 + tool_calls_recent_window)`
- `ref_complexity_bits = log2(1 + unique_reference_ids_recent_window)`
- `context_pressure_bits = 6.0 * context_used_ratio`

公式：

`H_hat = 0.35*action_complexity_bits + 0.30*tool_complexity_bits + 0.20*ref_complexity_bits + 0.15*context_pressure_bits`

### 容量先验（`C_hat`）

按模型的先验值：

- `deepseek_v3_2_chat = 3.9`
- `deepseek_v3_2_reasoner = 4.1`
- `deepseek_v4_pro = 3.5`
- `deepseek_v4_flash = 4.2`
- 回退值 `3.8`（用于其他 DeepSeek ID，包括未来发布的版本）

### 失败概率

使用滚动剖面字段：

- `final_slack`
- `min_slack`
- `violation_ratio`
- `slack_volatility`
- `slack_drop`

公式：

`z = -1.65*final_slack -0.85*min_slack +1.35*violation_ratio +0.70*slack_volatility +0.28*slack_drop -0.12`

`p_fail = sigmoid(z)`，截断到 `[0,1]`。

风险区间：

- 低：`p_fail <= low_risk_max`
- 中：`p_fail <= medium_risk_max`
- 高：其余情况

控制器显式启用时的动作映射：

- 低 -> `NoIntervention`
- 中 -> `TargetedContextRefresh`
- 高 + 严重动态（`min_slack <= severe_min_slack` 或
  `violation_ratio >= severe_violation_ratio`）-> `VerifyAndReplan`
- 其余高 -> `VerifyWithToolReplay`

## 检查点

启用后，引擎会在以下位置评估控制器策略：

1. 请求前检查点（组装 `MessageRequest` 之前）。
2. 工具后检查点（追加工具结果之后）。
3. 错误升级检查点（工具错误连击路径）。

## 干预

干预并不属于默认的 v0.7.5 V4 路径。默认路径是：追加消息、保持前缀缓存
复用、在接近真实模型压力时建议手动执行 `/compact`，并且只有在请求会超出
模型输入预算时才使用溢出恢复。

### `TargetedContextRefresh`

- 在可行时执行压缩（`compact_messages_safe`）。
- 压缩路径失败时回退为本地裁剪。
- 持久化规范状态。
- 用紧凑的规范 prompt + memory 指针替换长尾的活跃上下文。

### `VerifyWithToolReplay`

- 从最近的回合上下文中重放一次只读的关键工具调用。
- 追加验证说明，包含通过/失败结果 + diff 摘要。
- 若重放冲突/出错，标记为升级候选，并在当前回合内禁用重放。

### `VerifyAndReplan`

- 持久化规范快照。
- 清除易失的 prompt 尾部，同时保留最新的用户请求和最新的验证说明。
- 向系统 prompt 注入规范化的 replan 指令。
- 从紧凑的规范状态继续回合循环。

## 安全控制

- 每回合最多一次干预。
- refresh 与 replan 各有冷却期。
- 每回合的重放预算（`max_replay_per_turn`）。
- 控制器输入不可用时采取 fail-open 行为。
- 压缩/重放失败只记录日志；回合继续执行。

## 记忆存储

路径：

- `CODESMITH_CAPACITY_MEMORY_DIR`（若已设置）
- 否则为 `~/.codesmith/memory/<session_id>.jsonl`
- 回退：已有的 `~/.codesmith/memory/<session_id>.jsonl`，或需要时使用工作区本地的 `.codesmith` / 旧版 `.codesmith` 记忆路径

这些路径中的 `<session_id>` 是**持久线程 id**（`Session.id`，也以
`CODESMITH_THREAD_ID` 的形式暴露给 hooks）——它在 resume 之后仍然存在，
正是它让容量记忆具备跨会话连续性。它与临时的 `telemetry_session_id`
（以 `CODESMITH_SESSION_ID` 的形式暴露给 hooks）是有意区分的：后者在每次
构造会话时重新生成，从不写入磁盘。容量遥测事件携带临时 id；磁盘上的容量
记忆文件则以持久 id 为键。

记录字段：

- `id`, `ts`, `turn_index`, `action_trigger`
- `h_hat`, `c_hat`, `slack`, `risk_band`
- `canonical_state`
- `source_message_ids`
- 可选的 `replay_info`

加载工具支持获取最近 `K` 个快照用于恢复（rehydration）。

## 配置

`[capacity]` 键：

- `enabled`（默认 `false`）
- `low_risk_max`（默认 `0.50`）
- `medium_risk_max`（默认 `0.62`）
- `severe_min_slack`（默认 `-0.25`）
- `severe_violation_ratio`（默认 `0.40`）
- `refresh_cooldown_turns`（默认 `6`）
- `replan_cooldown_turns`（默认 `5`）
- `max_replay_per_turn`（默认 `1`）
- `min_turns_before_guardrail`（默认 `4`）
- `profile_window`（默认 `8`）
- `deepseek_v3_2_chat_prior`（默认 `3.9`）
- `deepseek_v3_2_reasoner_prior`（默认 `4.1`）
- `deepseek_v4_pro_prior`（默认 `3.5`）
- `deepseek_v4_flash_prior`（默认 `4.2`）
- `fallback_default_prior`（默认 `3.8`）

同样提供 `DEEPSEEK_CAPACITY_*` 系列等价的环境变量覆盖。
