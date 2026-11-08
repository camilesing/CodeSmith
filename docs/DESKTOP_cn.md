# CodeSmith 桌面端

CodeSmith 桌面端是一个 Tauri 2 应用：UI 整体 vendor 自一个上游开源客户端
（MIT；上游许可证与来源见 `desktop/ui/vendor/THIRD_PARTY_NOTICES.md`），
经机械重命名去品牌后，由翻译层桥接到 `codesmith serve --http`。
`--preset all` 在本应进入终端 REPL 的位置拉起的就是它
（见 [CLI_cn.md](CLI_cn.md) →「桌面 UI（preset `all`）」）。

## 架构

```
Tauri App (desktop/)
├─ WebView（vendored 客户端 UI，58 包闭包）
│   ├─ 我方 boot：__CODESMITH_BOOT__ 插件图 + 第一方品牌行
│   └─ 我方 connection 载体：一元 rpc.call 走 HTTP；所有流走 vendored
│        WebSocket mux（每客户端一条 socket）
│        POST /api/<endpoint> · ws /api/remote.mux ─► 环回桥 127.0.0.1:1421
└─ Rust src-tauri：客户端 RPC 桥（唯一由我方拥有的协议代码）
    ├─ 客户端 RPC 面：session/* settings/* workspace/* commands/* …
    ├─ 流 mux：每条逻辑流 open/item/end/cancel ↔ item/end/error；
    │   升级握手只放行页面源
    ├─ 事件映射器：runtime SSE envelope → 客户端线事件
    │   （每会话单调 seq、assistant-stream revision 严格 +1、
    │    surfaceOp 合成、数值化 turn/step 序号）
    ├─ $events waterfall：审批 / 用户问题经
    │   POST /v1/approvals/{id} 与 /v1/user-input/{thread}/{input} 回执
    └─ 子进程：`codesmith serve --http`（随机端口 + 一次性 token）
```

- 前端除 boot 装配与传输适配外不写新 UI 代码；像素来自 vendored 产物。
- 上行：客户端路由 → runtime_api REST（`POST /v1/threads/{id}/queue`、
  `…/steer`、`…/interrupt`、`PATCH /v1/threads/{id}`、`GET /v1/models` …）。
- 下行：后端 SSE 流折叠为客户端线事件与 assistant-stream 帧。
- 未实现的端点按 `gateway/method-unavailable` 语义返回错误态
  （客户端渲染错误态，不假装成功）。

## vendor 管线与去品牌

`desktop/scripts/vendor_client.py` 从上游 checkout（默认
`/Users/camile/Work/TypeScript/deepseek-harness`，可用 `--upstream` 或
`CODESMITH_DESKTOP_UPSTREAM` 覆盖）拷贝已构建的客户端闭包，依次应用：

1. **VENDOR_PATCHES**——精确字符串的产品文案补丁（上游字符串消失即
   fail-loud）。
2. **REWRITES**——作用于每个 vendored 文本文件的机械改写表（顺序敏感）：
   包作用域 `@deepseek-ai/dsh-*` → `@codesmith/*`、bundler 标识符、
   全局 `__DSH_*` → `__CODESMITH_*`、环境变量 `DSH_*` → `CODESMITH_*`、
   产品文案，以及小写 `dsh-` 前缀（CSS 变量、存储键、URL scheme）→
   `cds-`（**长度不变**，硬编码偏移如 `key.slice(21)` 保持有效）。
   改写表连同上游 commit 与逐文件哈希记录在 `ui/vendor/VENDOR.json`；
   重复执行是幂等的。

随后 `desktop/scripts/gen_boot_graph.mjs` 生成 boot 图
（`ui/src/boot-graph.generated.ts`）并把 classic-script bundle 暂存到
`ui/public/plugins/`。`desktop/scripts/check_brand.py` 强制"无上游品牌"
规则（出现品牌命中即退出码 1；仅 `THIRD_PARTY_NOTICES.md` 的版权行与
`VENDOR.json` 的来源记录豁免）。每次刷新 vendor 后运行：

```sh
python3 desktop/scripts/vendor_client.py && node desktop/scripts/gen_boot_graph.mjs
python3 desktop/scripts/check_brand.py
```

LICENSE 原样拷贝；MIT 声明与署名保留在 `ui/vendor/THIRD_PARTY_NOTICES.md`。

## 构建与运行

依赖：Rust（仓库 toolchain）、Node + pnpm、已构建的 `codesmith` 二进制
（`cargo build -p codesmith-cli`）。

```sh
# 1. vendor + boot 图（ui/vendor 已是最新时可跳过）
python3 desktop/scripts/vendor_client.py && node desktop/scripts/gen_boot_graph.mjs
# 2. 前端
cd desktop/ui && pnpm install && pnpm dev        # vite :1420
# 3. 桌面壳（桥 :1421，自行 spawn codesmith serve --http）
CODESMITH_DESKTOP_CODESMITH=<repo>/target/debug/codesmith \
  cargo run --manifest-path desktop/src-tauri/Cargo.toml
# 发布包（preset=all 查找的目标）
cargo tauri build --manifest-path desktop/src-tauri/Cargo.toml
```

环境变量：`CODESMITH_DESKTOP_CODESMITH`（后端二进制）、
`CODESMITH_DESKTOP_BRIDGE_PORT`（桥端口）、`CODESMITH_DESKTOP_APP`
（CLI 从哪里拉起应用）。只起浏览器预览（`pnpm dev` 不起壳）时回退到
内存 stub 传输，并显示可见的「离线演示模式」角标。

测试：`cargo test --manifest-path desktop/src-tauri/Cargo.toml`——映射器
（seq/revision 不变量、帧形状）、waterfall 映射、journal page 契约、流 mux
的帧契约与 Origin 闸门。

## CLI 集成

`--preset all`（或 `preset = "all"`）把 REPL 入口替换为桌面拉起
（`crates/cli/src/desktop.rs`）：应用查找顺序为 `[desktop] app_path` →
`$CODESMITH_HOME/desktop/codesmith-desktop` → 平台安装位置；缺失即
fail-loud 并附构建指引；`[desktop] enabled = false` 显式退出。被拉起的
应用收到指向拉起二进制的 `CODESMITH_DESKTOP_CODESMITH`。

## 已知限制

当前实现的已知限制：

- vendored 副本与上游的同步是手工的（`vendor_client.py` 记录来源
  commit）；没有实时跟踪。
- RPC 面是子集；未实现端点按 `gateway/method-unavailable` 返回错误态，
  不假装成功。会话重命名/复刻/搜索、归档/取消归档、会话置顶与侧栏手动
  排序、提示队列、技能目录、桌面设置文档、基于 `/v1/automations` 的
  任务页、控制流投影均已接通。
- turn 运行中发出的 prompt 停到运行时线程的队列上
  （`POST /v1/threads/{id}/queue`）：队列坞从 `inbox` 投影渲染它（运行时的
  `queue.updated` 事件，实时推送），排队的 prompt 在运行中的 turn 一结束就按
  FIFO 开始、每个 turn 一条（interrupt 不清队列）。`session/updateQueue` 桥接
  队列坞的三个动作：edit（纯文本——条目的附件被丢弃，队列坞对带附件的行禁用
  Edit）、remove、steer（把条目拉进运行中的 turn）。缺口：steer 带附件的行会
  被响亮拒绝（steer 仅文本）；`serve` 重启后队列仍在，但要等下一次 prompt 或
  turn 结束才开始（resume-idle）；排队的 turn 使用线程默认值，不带逐 prompt
  覆盖项。
- 侧栏置顶与手动排序是桌面壳自有状态，持久化在
  `~/.codesmith/desktop/workspace.json`，不写回运行时：置顶会话排在分组
  前列（最近置顶在前），手动排序保持相对顺序，从未拖动过的会话按运行时
  近期顺序跟在其后。斜杠命令目录只收 `/compact`，经 commands/execute
  桥到运行时的手动压缩：调用必须无参数、无附件（与上游准入一致）；
  执行结果只代表运行时接受了压缩——压缩本身作为异步 turn 运行，由
  转写节点（映射后的 `context_compaction` 系统消息）携带结果；会话忙时
  把运行时的冲突文案作为命令错误通知呈现。REPL 的其余斜杠命令是
  REPL 本地的，不进目录，对应 "/" 开头的行照常作为普通 prompt 提交。
- 上游独有概念（账户/余额、插件市场、后台任务列表、右侧栏
  终端/文件/浏览器/预览、子代理视图、目标条、计划预览）不仿真——对应
  插件不进 boot roster 或返回空态。
- 附件上传已端到端桥接。输入框的上传先暂存在桥里（`fileUploads/upload`
  与原始字节路由 `api/session/uploadFileBinary`，由页面安装的
  `__CODESMITH_FILE_UPLOAD__` 钩子改道到桥的 origin），`session/prompt`
  把暂存回执解析为 runtime turn API 的内联 `attachments[]`。图片以原生
  image 块到达模型（魔数校验、尺寸从头解析）；其余文件经 REPL 同一套
  摄取管道折进消息（内联文本，或模型可按需 `read_file` 的持久存储路径
  引用行）。已结算消息保留缩略图/文件条（runtime 把每个附件记录在 user
  item 上；`session/attachment` 从持久存储取回字节，重启后历史仍可渲染）。
  限额与缺口：单文件 10MB、每条消息 8 个、音频/视频拒绝（在上传时以
  业务错误呈现）；回执是内存态——桌面重启丢弃已暂存未发送的上传，需
  重新附上；steer 不能带附件（runtime 的 steer 路由纯文本，桥会响亮
  拒绝）；无上传进度条；无 EXIF 方向修正或图片归一化（字节原样传输）。
- 任务页：automations 只接受两种 RRULE 形态（`FREQ=HOURLY` 带
  INTERVAL/BYDAY，`FREQ=WEEKLY` 带 BYDAY/BYHOUR/BYMINUTE，按机器本地
  时区求值）。任务页的每日/每周/整小时间隔/cron 形态会编译过去；一次性
  定时、不足一小时的间隔、异时区、其余 cron 形态在创建/编辑时响亮失败，
  而不是静默改期。"让模型创建"只打开会话——模型没有注册日程的工具，
  请用手动表单。
- 工具调用的输出增量不流式；工具节点由持久化结果落定。工具名在桥的
  事件映射处规范化为 vendored 卡片词汇（`exec_shell` → bash 卡、
  `read_file` → read 卡……）；运行时记录保留 CodeSmith 真名，且回放的
  工具调用丢失参数（持久化条目只带结果元数据）。
- 用户问题没有 HTTP 取消路由，未作答的问题 waterfall 等运行时自身的
  截止时间；问题请求不带工具调用 id，输入框的问题卡不渲染倒计时。
- `$events` waterfall 遵循上游 gateway 契约：每条流有自己的
  `clientId`，仍待决的请求会重放给新连接的流，迟到/重复的
  `$events/result` 是幂等 no-op（`delivered:false`，含运行时 404）。决定
  送达后紧跟该请求的 cancel 帧——各个客户端上答题卡的退役靠的就是这一
  帧。检测不依赖客户端正在 follow 该会话：每次 prompt 都为会话挂一个
  后台 watcher（线程事件流），自行把审批/问题浮出来；队列启动的 turn 会
  重新挂上它。早于 watcher 的 prompt（外部 HTTP 客户端、重启前的 turn）
  仍需要 follow 流才会浮现。
- 桌面会话继承配置顶层 `allow_shell`（默认 `false`，
  GHSA-72w5-pf8h-xfp4）：关闭时模型没有 shell 工具，审批卡来自写工具
  （`write_file`/`edit_file`）。桥面绝不擅自翻转该安全默认值，由用户在
  配置里显式开启。
- 流统一走每客户端一条 WebSocket（`/api/remote.mux`，即 vendored mux
  客户端的协议；一元调用仍是 HTTP POST，与上游一致），因此无论开多少条
  流，一个客户端只占一条桥连接——原 fetch+NDJSON 载体每条流占一个连接，
  会占满 Chromium 单 host 6 连接的池并拖慢刷新。WebSocket 不受 CORS
  约束，故升级握手只放行页面源（无 Origin 的本地客户端与 HTTP 路由一样
  放行）。mux 不读上行（没有桥接端点接收客户端 item；上游唯一的上行端点
  ——侧栏终端——不仿真），也不发 ping 心跳（环回：页面消失即关闭其
  socket，进而中止其承载的全部流）。
- 输入框的乐观副本靠回显的 `source.rpcId`（客户端发的是 `requestId`）
  退役。运行时把该 id 记在排队条目和落定的 user item 上
  （`metadata.rpc_id`），因此按身份匹配：队列 steer 或 remove 不会把它错挪到
  别的消息上，每个 follow 该会话的客户端看到的都是同一个戳记。
- 输入框的模型座位由会话的 `modelSelection` 投影加共享目录解析：空会话
  的日志以游标 −1 打开（seq 从 0 编号），投影挂进程级 seq 时钟（运行时
  不为切模型记日志事件），控制帧的 `changes` 是 `{key, val}` 数组。座位
  里切模型会持久化线程模型并推送实时投影帧；目录里没有的模型 id 保留
  原 id，座位读作不可路由。
- 桌面离线 E2E 走环回 provider：协议时序用脚本化 mock，真实生成用本地
  真模型——把 Qwen2.5-1.5B-Instruct 从本机 HF 缓存导入 ollama 0.23.2
  （`ollama create` 指向 safetensors 目录，配一份手写的渲染 `.Tools` 的
  模板；已拉取的 `qwen2` 原模板没有 tools 块，端点每回合答 400）。真
  模型普通回合已端到端点验（prompt → 流式 → 落定 → 重放，重放时刻与
  持久记录逐帧一致），两条长期约束仍在：codesmith 首回合约 5 万 token
  的请求超出本机所有本地模型声明的 32k 上下文（ollama 从头截断，模型
  凭幸存的尾部仍能作答）；导入模型的 `<tool_call>` 块只作纯文本返回
  （没有结构化 `tool_calls`），且导入器拒绝 `Qwen3ForCausalLM`——真
  模型工具回合（真实工具调用之上的活审批卡）因此仍未点验。
- 去品牌改写是文本级的（审计说明见 `vendor_client.py` 模块注释）：未来
  上游若出现依赖品牌字符串长度的偏移量，脚本不会发现——
  `check_brand.py` 管品牌命中，升级上游 commit 时需人工复核偏移。
- 与上游客户端的像素一致性是人工截图比对
  （`desktop/screenshots/baseline/`），未自动化。我方四张基线（空态/
  fixture × 明暗，1200×800）已捕获；上游对照待人工——2026-11-07 的尝试
  被 Electron 二进制下载失败、上游 host CLI 未构建、本机窗口捕获被系统
  涂黑三重阻断（证据见 `desktop/screenshots/baseline/README.md`）。
