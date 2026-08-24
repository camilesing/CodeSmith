# 提供商注册表

本注册表描述已接入当前 CodeSmith 代码库的提供商行为。它刻意保持保守：
已交付的条目仅限于代码已知的提供商 ID、配置键、认证路径、base URL、
模型解析和能力元数据。

DeepSeek 仍然是一等默认提供商。NVIDIA NIM、OpenRouter、
Volcengine Ark、Xiaomi MiMo、Novita、Fireworks、SiliconFlow、通用
OpenAI 兼容端点、自托管运行时以及 Moonshot/Kimi 是附加路由，
用于让同一终端框架对接其他托管或本地模型端点。Hugging Face Inference
Providers 是规划中的附加开源模型路由层；它们在当前检出中还不是原生
提供商。

需要保持同步的来源：

- `crates/config/src/lib.rs` - 共享的提供商 ID、默认值、环境变量优先级。
- `crates/tui/src/config.rs` - TUI 提供商 ID、提供商能力元数据
  以及提供商特定的环境变量处理。
- `crates/agent/src/lib.rs` - 供 `codesmith model list` 和
  `codesmith model resolve` 使用的静态 `ModelRegistry`。
- `config.example.toml` 和 `docs/CONFIGURATION.md` - 面向用户的配置
  示例和环境变量参考。
- `scripts/check-provider-registry.py` - 针对规范提供商 ID、活跃 TUI
  提供商 ID、TOML 表名、静态注册表行和已文档化默认值的漂移检查。

## 提供商选择

规范的提供商 ID 为：

`deepseek`、`nvidia-nim`、`openai`、`atlascloud`、`wanjie-ark`、`volcengine`、
`openrouter`、`xiaomi-mimo`、`novita`、`fireworks`、`siliconflow`、`moonshot`、
`sglang`、`vllm` 和 `ollama`。

使用以下任一入口选择提供商：

- CLI：`codesmith --provider <id>`
- TUI：`/provider <id>` 或提供商选择器
- 环境变量：`CODESMITH_PROVIDER=<id>`；`DEEPSEEK_PROVIDER=<id>` 是旧版别名
- 配置：`provider = "<id>"`

`deepseek-cn`、`deepseek_china`、`deepseekcn` 和 `deepseek-china` 被接受为
`deepseek` 的旧版别名。它们不会选择不同的官方主机；
DeepSeek 在全球使用相同的官方 API 主机。

新建的共享配置写入 `~/.codesmith/config.toml`。已有的
`~/.deepseek/config.toml` 文件出于兼容性仍会被读取。

## 认证与环境变量规则

对于托管提供商，`codesmith auth set --provider <id>` 会保存该提供商的
API 密钥。API 密钥环境变量是排在已保存配置和密钥环凭据之后的回退输入；
显式的进程级 `--api-key` 在该次启动中仍然优先。

对于 base URL 和模型选择，优先使用：

- `CODESMITH_BASE_URL` / `CODESMITH_MODEL`，作用于当前活跃提供商。
- 下文列出的提供商特定 base URL/模型环境变量。
- `DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL` 和 `DEEPSEEK_DEFAULT_TEXT_MODEL` 作为
  旧版别名。

非本地的 `http://` base URL 会被拒绝，除非设置了
`DEEPSEEK_ALLOW_INSECURE_HTTP=1`。环回 HTTP URL 被允许用于
自托管运行时。

## 自定义 DeepSeek 兼容端点

大多数自定义 DeepSeek 兼容部署可以使用现有的提供商 ID。
不要创建 `[providers.deepseek_custom]`；提供商表名是固定的。
应选择最接近的已交付路由并覆盖其端点/模型：

- DeepSeek 兼容的托管 API：保持 `provider = "deepseek"` 并设置
  `[providers.deepseek].base_url` 和 `[providers.deepseek].model`，或使用
  `DEEPSEEK_BASE_URL` 和 `DEEPSEEK_MODEL` 启动。
- 通用 OpenAI 兼容网关：使用 `provider = "openai"` 并设置
  `[providers.openai].base_url` 和 `[providers.openai].model`，或使用
  `OPENAI_BASE_URL` 和 `OPENAI_MODEL` 启动。自定义网关 URL 若未指定任何
  显式模型，会在启动时快速失败 —— 网关的模型目录不包含
  提供商默认模型。
- 本地 OpenAI 兼容运行时：使用 `provider = "vllm"`、`"sglang"` 或
  `"ollama"`，并配置匹配的提供商特定 base URL/模型值。

DeepSeek 兼容主机的用户配置示例：

```toml
provider = "deepseek"

[providers.deepseek]
api_key = "YOUR_API_KEY"
base_url = "https://your-provider.example/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"
```

通用网关的用户配置示例：

```toml
provider = "openai"

[providers.openai]
api_key = "YOUR_GATEWAY_API_KEY"
base_url = "https://gateway.example/v1"
model = "your-deepseek-compatible-model"
```

请将 `provider`、`api_key` 和 `base_url` 保留在用户配置或进程
环境中。项目本地配置覆盖层刻意无法设置这些键，
因此仓库无法静默地将提示词或凭据重定向到另一个
端点。

## 已交付的提供商

| 提供商 ID | TOML 表 | 认证环境变量 | Base URL 环境变量与默认值 | 默认或静态模型 | 备注 |
| --- | --- | --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | `DEEPSEEK_API_KEY` | `CODESMITH_BASE_URL` / `DEEPSEEK_BASE_URL`；默认 `https://api.deepseek.com/beta` | `deepseek-v4-pro`、`deepseek-v4-flash`；兼容别名 `deepseek-chat`、`deepseek-reasoner` | 一等默认提供商。Beta URL 启用 strict tool mode、chat prefix completion 和 FIM completion。显式设置 `https://api.deepseek.com` 或 `/v1` 可退出仅限 beta 的功能。 |
| `nvidia-nim` | `[providers.nvidia_nim]` | `NVIDIA_API_KEY`、`NVIDIA_NIM_API_KEY`、回退 `DEEPSEEK_API_KEY` | `NVIDIA_NIM_BASE_URL`、`NIM_BASE_URL`、`NVIDIA_BASE_URL`；默认 `https://integrate.api.nvidia.com/v1` | `deepseek-ai/deepseek-v4-pro`、`deepseek-ai/deepseek-v4-flash` | 通过 NVIDIA NIM 托管的 DeepSeek V4。TUI 配置路径接受 `NVIDIA_NIM_MODEL`。 |
| `openai` | `[providers.openai]` | `OPENAI_API_KEY` | `OPENAI_BASE_URL`；默认 `https://api.openai.com/v1` | 注册表条目：`gpt-5`、`deepseek-v4-pro`、`deepseek-v4-flash`；默认配置模型 `gpt-5` | 用于网关和自定义端点的通用 OpenAI 兼容路由。对显式的第三方 OpenAI 兼容路由请使用它，而不是发明新的提供商 ID。接受 `OPENAI_MODEL`。自定义 `OPENAI_BASE_URL` 若未指定显式模型，会在启动时快速失败。 |
| `atlascloud` | `[providers.atlascloud]` | `ATLASCLOUD_API_KEY` | `ATLASCLOUD_BASE_URL`；默认 `https://api.atlascloud.ai/v1` | `deepseek-ai/deepseek-v4-flash`、`deepseek-ai/deepseek-v4-pro` | OpenAI 兼容的托管路由。TUI 配置路径接受 `ATLASCLOUD_MODEL`，静态 `ModelRegistry` 中包含用于 CLI 模型解析的 AtlasCloud 回退行。 |
| `wanjie-ark` | `[providers.wanjie_ark]` | `WANJIE_ARK_API_KEY`、`WANJIE_API_KEY`、`WANJIE_MAAS_API_KEY` | `WANJIE_ARK_BASE_URL`、`WANJIE_BASE_URL`、`WANJIE_MAAS_BASE_URL`；默认 `https://maas-openapi.wanjiedata.com/api/v1` | `deepseek-reasoner` | OpenAI 兼容的托管路由。接受 `WANJIE_ARK_MODEL`、`WANJIE_MODEL` 和 `WANJIE_MAAS_MODEL`。 |
| `volcengine` | `[providers.volcengine]` | `VOLCENGINE_API_KEY`、`VOLCENGINE_ARK_API_KEY`、`ARK_API_KEY` | `VOLCENGINE_BASE_URL`、`VOLCENGINE_ARK_BASE_URL`、`ARK_BASE_URL`；默认 `https://ark.cn-beijing.volces.com/api/coding/v3` | `DeepSeek-V4-Pro`、`DeepSeek-V4-Flash` | Volcengine/火山引擎 Ark OpenAI 兼容编码端点。接受 `VOLCENGINE_MODEL` 和 `VOLCENGINE_ARK_MODEL`。 |
| `openrouter` | `[providers.openrouter]` | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL`；默认 `https://openrouter.ai/api/v1` | `deepseek/deepseek-v4-pro`、`deepseek/deepseek-v4-flash`；近期大型 ID 包括 `arcee-ai/trinity-large-thinking`、`qwen/qwen3.7-max`、`xiaomi/mimo-v2.5-pro`、`qwen/qwen3.6-35b-a3b`、`google/gemma-4-31b-it`、`z-ai/glm-5.1`、`moonshotai/kimi-k2.6` | 附加的开源模型路由层。它不替代 DeepSeek；它让用户在选择时可以通过 OpenRouter 路由受支持的模型 ID。 |
| `xiaomi-mimo` | `[providers.xiaomi_mimo]` | `XIAOMI_MIMO_API_KEY`、`XIAOMI_API_KEY`、`MIMO_API_KEY` | `XIAOMI_MIMO_BASE_URL`、`MIMO_BASE_URL`；默认 `https://api.xiaomimimo.com/v1` | `mimo-v2.5-pro`、`mimo-v2.5` | 小米 MiMo OpenAI 兼容 chat completions 路由。它发送 `max_completion_tokens` 并使用 MiMo 的 `thinking` 字段进行推理控制。 |
| `novita` | `[providers.novita]` | `NOVITA_API_KEY` | `NOVITA_BASE_URL`；默认 `https://api.novita.ai/v1` | `deepseek/deepseek-v4-pro`、`deepseek/deepseek-v4-flash` | 用于 DeepSeek 模型 ID 的 OpenAI 兼容托管路由。使用配置或 `CODESMITH_MODEL` / `DEEPSEEK_MODEL` 进行模型覆盖。 |
| `fireworks` | `[providers.fireworks]` | `FIREWORKS_API_KEY` | `FIREWORKS_BASE_URL`；默认 `https://api.fireworks.ai/inference/v1` | `accounts/fireworks/models/deepseek-v4-pro` | OpenAI 兼容的托管路由。使用配置或 `CODESMITH_MODEL` / `DEEPSEEK_MODEL` 进行模型覆盖。 |
| `siliconflow` | `[providers.siliconflow]` | `SILICONFLOW_API_KEY` | `SILICONFLOW_BASE_URL`；默认 `https://api.siliconflow.com/v1` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | OpenAI 兼容的托管路由。官方文档使用 `.com` 端点；需要区域端点的用户可显式设置 `https://api.siliconflow.cn/v1`。接受 `SILICONFLOW_MODEL`。推理别名 `deepseek-reasoner` 和 `deepseek-r1` 映射到 Pro；`deepseek-chat` 和 `deepseek-v3` 映射到 Flash。 |
| `moonshot` | `[providers.moonshot]` | `MOONSHOT_API_KEY`、`KIMI_API_KEY` | `MOONSHOT_BASE_URL`、`KIMI_BASE_URL`；默认 `https://api.moonshot.ai/v1` | `kimi-k2.6`；Kimi Code 路径在 `https://api.kimi.com/coding/v1` 使用 `kimi-for-coding` | Moonshot/Kimi 路由。接受 `MOONSHOT_MODEL`、`KIMI_MODEL_NAME` 和 `KIMI_MODEL`。`[providers.moonshot] auth_mode = "kimi_oauth"` 会在存在时读取 Kimi CLI OAuth 凭据。 |
| `sglang` | `[providers.sglang]` | 可选 `SGLANG_API_KEY` | `SGLANG_BASE_URL`；默认 `http://localhost:30000/v1` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | 自托管 OpenAI 兼容路由。localhost 部署通常省略认证。接受 `SGLANG_MODEL`。 |
| `vllm` | `[providers.vllm]` | 可选 `VLLM_API_KEY` | `VLLM_BASE_URL`；默认 `http://localhost:8000/v1` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | 自托管 vLLM OpenAI 兼容路由。localhost 部署通常省略认证。接受 `VLLM_MODEL`。 |
| `ollama` | `[providers.ollama]` | 可选 `OLLAMA_API_KEY` | `OLLAMA_BASE_URL`；默认 `http://localhost:11434/v1` | `deepseek-coder:1.3b`；提供商提示的自定义 tag 直接透传 | 自托管 Ollama OpenAI 兼容路由。localhost 部署通常省略认证。接受 `OLLAMA_MODEL`。 |

### 小米 MiMo 说明

`xiaomi-mimo` 默认使用 `mimo-v2.5-pro` 进行长上下文推理和编码
工作，同时静态注册表还暴露 `mimo-v2.5`。小米当前的
[图像理解指南](https://platform.xiaomimimo.com/docs/en-US/usage-guide/multimodal-understanding/image-understanding)
在图像输入中包含 `mimo-v2.5`。CodeSmith 通过独立的
`[vision_model]` / `image_analyze` 路径提供图像分析；在将 MiMo 用于
视觉时请将该模型设置为
`mimo-v2.5`。

### 近期 OpenRouter 大型模型

OpenRouter completions 和静态注册表行包含 2026 年 4 月及以后
通过 OpenRouter 模型元数据验证的大型开放权重或开放标注模型：
`arcee-ai/trinity-large-thinking`、`qwen/qwen3.6-35b-a3b`、
`qwen/qwen3.6-27b`、`xiaomi/mimo-v2.5-pro`、`xiaomi/mimo-v2.5`、
`moonshotai/kimi-k2.6`、`z-ai/glm-5.1`、`tencent/hy3-preview`、
`google/gemma-4-31b-it`、`google/gemma-4-26b-a4b-it` 以及
`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`。`qwen/qwen3.7-max`
也被包含在内，因为它是当前用户请求的 OpenRouter 大型模型，
但它被视为托管的 Qwen 模型，而不是被记录为开放权重。

## 静态模型注册表

`codesmith model list` 和 `codesmith model resolve` 使用
`crates/agent/src/lib.rs` 中的静态注册表。这与实时的 `/models` 发现不同。
当端点支持模型列表时，使用 `/models` 或 `codesmith models` 从当前活跃的
API 端点获取模型 ID。

| 提供商 | 静态注册表条目 | 工具调用 | 注册表推理标志 |
| --- | --- | --- | --- |
| `deepseek` | `deepseek-v4-pro`、`deepseek-v4-flash` | 是 | 是 |
| `nvidia-nim` | `deepseek-ai/deepseek-v4-pro`、`deepseek-ai/deepseek-v4-flash` | 是 | 是 |
| `openai` | `gpt-5`、`deepseek-v4-pro`、`deepseek-v4-flash` | 是 | 是 |
| `atlascloud` | `deepseek-ai/deepseek-v4-flash`、`deepseek-ai/deepseek-v4-pro` | 是 | 是 |
| `wanjie-ark` | `deepseek-reasoner` | 是 | 是 |
| `volcengine` | `DeepSeek-V4-Pro`、`DeepSeek-V4-Flash` | 是 | 是 |
| `openrouter` | `deepseek/deepseek-v4-pro`、`deepseek/deepseek-v4-flash`、`arcee-ai/trinity-large-thinking`、`qwen/qwen3.7-max`、`xiaomi/mimo-v2.5-pro`、`xiaomi/mimo-v2.5`、`qwen/qwen3.6-35b-a3b`、`qwen/qwen3.6-27b`、`moonshotai/kimi-k2.6`、`z-ai/glm-5.1`、`tencent/hy3-preview`、`google/gemma-4-31b-it`、`google/gemma-4-26b-a4b-it`、`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | 是 | 是 |
| `xiaomi-mimo` | `mimo-v2.5-pro`、`mimo-v2.5` | 是 | 是 |
| `novita` | `deepseek/deepseek-v4-pro`、`deepseek/deepseek-v4-flash` | 是 | 是 |
| `fireworks` | `accounts/fireworks/models/deepseek-v4-pro` | 是 | 是 |
| `siliconflow` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `moonshot` | `kimi-k2.6` | 是 | 是 |
| `sglang` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `vllm` | `deepseek-ai/DeepSeek-V4-Pro`、`deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `ollama` | `deepseek-coder:1.3b`；当提供商提示为 `ollama` 时自定义 tag 直接透传 | 是 | 否 |

AtlasCloud 保持与配置层相同的默认模型，并为 Pro 和 Flash 行添加
提供商作用域的别名。其他 AtlasCloud 模型 ID
仍应通过 `ATLASCLOUD_MODEL`、配置或在可用时通过实时模型
列表来选择。

## 能力元数据

`codesmith-tui doctor --json` 暴露 `capability` 对象。它是静态
元数据，不是实时 API 探测。当前字段为：

`resolved_provider`、`resolved_model`、`context_window`、`max_output`、
`thinking_supported`、`cache_telemetry_supported` 和 `request_payload_mode`。

目前所有已交付的提供商都使用 Chat Completions 请求载荷模式。

| 提供商/模型类别 | 上下文窗口 | 最大输出元数据 | 推理支持 | 缓存遥测 | FIM 端点 |
| --- | --- | --- | --- | --- | --- |
| DeepSeek V4（`deepseek-v4-pro`、`deepseek-v4-flash`） | 1,000,000 | 384,000 | 是 | 是 | 仅 DeepSeek beta |
| DeepSeek 兼容别名（`deepseek-chat`、`deepseek-reasoner`） | 1,000,000 | 384,000 | 是 | 是 | 仅 DeepSeek beta |
| NVIDIA NIM V4 注册表模型 | 1,000,000 | 384,000 | 是 | 是 | 代码中未记录 |
| Volcengine Ark V4 模型 ID | 1,000,000 | 384,000 | 是 | 是 | 代码中未记录 |
| OpenRouter、Novita、Fireworks、SiliconFlow、SGLang 和 vLLM 的 V4 模型 ID | 1,000,000 | 384,000 | 是 | 否 | 代码中未记录 |
| 小米 MiMo 模型 | 1,000,000 | 128,000 | 是 | 否 | 代码中未记录 |
| 万捷 Ark `reasoner` / `r1` 模型 ID | 128,000 | 4,096 | 是 | 否 | 代码中未记录 |
| 通用 `openai`、AtlasCloud 和 Moonshot/Kimi | 128,000 | 4,096 | doctor 能力元数据中为否 | 否 | 代码中未记录 |
| Ollama | 8,192 | 4,096 | 否 | 否 | 代码中未记录 |
| 其他已识别的 DeepSeek 模型 ID | 128,000，除非模型名带有显式 `Nk` 提示 | 4,096 | 否，除非匹配 V4/reasoner 逻辑 | 仅 DeepSeek/NIM | 仅 DeepSeek beta |

工具调用支持由静态 `ModelRegistry` 以及端点接受 OpenAI 兼容 `tools`
载荷的能力单独跟踪。自定义的
OpenAI 兼容或本地端点即使 CodeSmith
能够发送 schema，仍可能拒绝工具调用。

DeepSeek 兼容别名 `deepseek-chat` 和 `deepseek-reasoner` 映射到
`deepseek-v4-flash` 能力元数据。原定的 2026-07-24 退役日期已过但未执行
移除——别名仍可解析，且未承诺新的移除日期。

## 漂移检查

在更改提供商 ID、提供商 TOML 表、静态模型
注册表行或提供商默认字符串之前运行：

```bash
python3 scripts/check-provider-registry.py
```

检查在以下情况失败：

- `docs/PROVIDERS.md` 遗漏了规范的 `ProviderKind::as_str()` ID。
- `crates/tui/src/config.rs` 的 `ApiProvider::as_str()` 与
  `ProviderKind::as_str()` 不一致（显式的 `deepseek-cn` 旧版别名除外）。
- 已交付提供商表遗漏或新增了 `[providers.*]` TOML 表。
- 静态模型注册表表与 `crates/agent/src/lib.rs` 使用的
  提供商发生漂移。
- `crates/tui/src/config.rs` 中的提供商默认模型或 base URL 常量
  不再在此处被提及。

## 已规划、尚未交付

这些条目属于 v0.8.47 提供商抽象里程碑或相关的
提供商文档工作，但它们在当前检出中不是原生的已交付行为：

- `codesmith-agent` 中统一的 `Provider` trait，负责环境变量优先级、
  密钥解析、base URL 规范化、认证头构造以及
  提供商元数据。这些职责目前仍分散在
  `crates/config`、`crates/secrets`、`crates/tui/src/config.rs` 以及
  `crates/providers` 中的提供商客户端。
- 原生的 Hugging Face 提供商，例如 `[providers.huggingface]`。
- 原生的 Hugging Face 认证环境变量，例如 `HF_TOKEN` 或 `HUGGINGFACE_API_KEY`。
- 默认的 Hugging Face 路由器 base URL，例如
  `https://router.huggingface.co/v1`。
- 选择器中的 Hugging Face 模型护照元数据，包括许可证、基础
  模型、上下文长度、聊天模板、工具调用支持、推理支持
  以及受限/私有状态。

在原生 Hugging Face 支持落地之前，用户只能通过通用的 `openai`
提供商访问显式配置的 Hugging Face 兼容 OpenAI 路由。这是一条
用户显式选择的路由，不是内置的 Hub 发现，
也不是 DeepSeek 的替代品。
