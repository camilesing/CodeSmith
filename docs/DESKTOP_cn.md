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
├─ WebView（vendored 客户端 UI，46 包闭包）
│   ├─ 我方 boot：__CODESMITH_BOOT__ 插件图 + 第一方品牌行
│   └─ 我方 connection 载体：fetch+NDJSON 传输（无 WebSocket）
│        rpc.call / rpc.open ─► 环回桥 http://127.0.0.1:1421
└─ Rust src-tauri：客户端 RPC 桥（唯一由我方拥有的协议代码）
    ├─ 客户端 RPC 面：session/* settings/* workspace/* $events follow…
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
（seq/revision 不变量、帧形状）、waterfall 映射、journal page 契约。

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
  不假装成功（`session/updateQueue`、fork/rename/search 等）。
- 上游独有概念（账户/余额、插件市场、日程、deliverables、附件上传）
  不仿真——对应插件不进 boot roster 或返回空态。
- 工具调用的输出增量不流式；工具节点由持久化结果落定。用户问题没有
  HTTP 取消路由，未作答的问题 waterfall 等运行时自身的截止时间。
- 去品牌改写是文本级的（审计说明见 `vendor_client.py` 模块注释）：未来
  上游若出现依赖品牌字符串长度的偏移量，脚本不会发现——
  `check_brand.py` 管品牌命中，升级上游 commit 时需人工复核偏移。
- 与上游客户端的像素一致性是人工截图比对（`desktop/screenshots/`），
  未自动化。
