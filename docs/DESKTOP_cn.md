# CodeSmith 桌面端

CodeSmith 桌面端是一个 Tauri 2 应用：UI 整体 vendor 自一个上游开源客户端
（MIT；上游许可证与来源见 `desktop/ui/vendor/THIRD_PARTY_NOTICES.md`），
经机械重命名去品牌后，由翻译层桥接到 `codesmith serve --http`。
`--preset all` 在本应进入终端 REPL 的位置拉起的就是它
（见 [CLI_cn.md](CLI_cn.md) →「桌面 UI（preset `all`）」）。

设计 spec：`docs/superpowers/specs/2026-11-06-codesmith-desktop-design.md`。

## 架构

```
Tauri App (desktop/)
├─ WebView（vendored 客户端 UI，58 包闭包）
│   ├─ 我方 boot：__CODESMITH_BOOT__ 插件图 + 第一方品牌行
│   └─ 我方 connection 载体：一元 rpc.call 走 HTTP；所有流走 vendored
│        WebSocket mux（每客户端一条 socket）
│        POST /api/<endpoint> · ws /api/remote.mux ─► 环回桥 127.0.0.1:1421
└─ Rust src-tauri：客户端 RPC 桥（唯一由我方拥有的协议代码）
    ├─ 客户端 RPC 面：session/* settings/* workspace/* $events follow…
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
- 上行：客户端路由 → runtime_api REST（`POST /v1/threads/{id}/turns`、
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

抄自设计 spec §10，加上实现新增：

- vendored 副本与上游的同步是手工的（`vendor_client.py` 记录来源
  commit）；没有实时跟踪。
- RPC 面是子集；未实现端点按 `gateway/method-unavailable` 返回错误态，
  不假装成功（`session/updateQueue`、会话置顶等）。会话重命名/复刻/
  搜索、归档/取消归档、技能目录、桌面设置文档、基于 `/v1/automations`
  的任务页、控制流投影均已接通。
- 上游独有概念（账户/余额、插件市场、后台任务列表、附件上传、右侧栏
  终端/文件/浏览器/预览、子代理视图、目标条、计划预览）不仿真——对应
  插件不进 boot roster 或返回空态。
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
  帧。检测不依赖客户端正在 follow 该会话：每次排队的 turn 都为会话挂一个
  后台 watcher（线程事件流），自行把审批/问题浮出来。早于 watcher 的
  prompt（外部 HTTP 客户端、重启前的 turn）仍需要 follow 流才会浮现。
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
  退役。运行时在 turn 开始时就发出该回显，故桥在运行时接受 prompt 之前
  就把 id 入队。每个 id 只给一条回显盖戳（最先看到它的 follow 循环）：
  两个客户端同时 follow 同一会话时，发送方可能拿不到戳记，其消息渲染
  两份。
- 输入框的模型座位由会话的 `modelSelection` 投影加共享目录解析：空会话
  的日志以游标 −1 打开（seq 从 0 编号），投影挂进程级 seq 时钟（运行时
  不为切模型记日志事件），控制帧的 `changes` 是 `{key, val}` 数组。座位
  里切模型会持久化线程模型并推送实时投影帧；目录里没有的模型 id 保留
  原 id，座位读作不可路由。
- 去品牌改写是文本级的（审计说明见 `vendor_client.py` 模块注释）：未来
  上游若出现依赖品牌字符串长度的偏移量，脚本不会发现——
  `check_brand.py` 管品牌命中，升级上游 commit 时需人工复核偏移。
- 与上游客户端的像素一致性是人工截图比对
  （`desktop/screenshots/baseline/`），未自动化。我方四张基线（空态/
  fixture × 明暗，1200×800）已捕获；上游对照待人工——2026-11-07 的尝试
  被 Electron 二进制下载失败、上游 host CLI 未构建、本机窗口捕获被系统
  涂黑三重阻断（证据见 `desktop/screenshots/baseline/README.md`）。
