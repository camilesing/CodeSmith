# CodeSmith 桌面端

CodeSmith 桌面端是一个 Tauri 2 应用：UI 整体 vendor 自一个上游开源客户端
（MIT；上游许可证与来源见 `desktop/ui/vendor/THIRD_PARTY_NOTICES.md`），
经机械重命名去品牌后，由翻译层桥接到 `codesmith serve --http`。
`--preset all` 在本应进入终端 REPL 的位置拉起的就是它
（见 [CLI_cn.md](CLI_cn.md) →「桌面 UI（preset `all`）」）。

## 架构

```
Tauri App (desktop/)
├─ WebView（vendored 客户端 UI，75 包闭包）
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

`desktop/scripts/vendor_client.py` 从本地上游 checkout（经 `--upstream` 或
`CODESMITH_DESKTOP_UPSTREAM` 环境变量定位）拷贝已构建的客户端闭包，
依次应用：

1. **VENDOR_PATCHES**——精确字符串的产品文案补丁（上游字符串消失即
   fail-loud）。
2. **REWRITES**——作用于每个 vendored 文本文件的机械改写表（顺序敏感）：
   上游包作用域 → `@codesmith/*`、bundler 标识符、上游全局 →
   `__CODESMITH_*`、上游环境变量名 → `CODESMITH_*`、产品文案，以及小写
   上游前缀（CSS 变量、存储键、URL scheme）→ `cds-`（**长度不变**，
   硬编码偏移如 `key.slice(21)` 保持有效）。改写表（含精确源字符串）
   连同上游 commit 与逐文件哈希记录在 `ui/vendor/VENDOR.json`；
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
# 1. vendor + boot 图。两侧任一漂移都要重跑：ui/public/plugins（暂存的
#    bundle）是 gitignore 的，vendor 树可能已最新而暂存是旧的——缺的
#    bundle 启动时 404，对应插件静默不激活（审批卡就在其中）。
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
内存 stub 传输，并显示可见的「离线演示模式」角标。stub 服务全部设置命名
空间——`ui-settings-general`、`locale`、`ui-settings`、`permission`、
`ui-theme`、`ui-conversation`、`ui-chat`——按桥端的 schema 提供内存写入，
所以开关、步进器和菜单选项都能生效，并在关闭设置后保留；权限 catalog 的
标签跟随内存里的语言设置。此外还提供 `session/events` 与空的 `@` 文件源，
让这些页面在无壳时也能渲染。

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
  排序、提示队列、技能目录、桌面设置文档（主题、字体、回车行为、转录视
  图、语言、开发者工具、新建会话的权限默认值）、基于
  `/v1/automations` 的任务页、控制流投影、计划页（`session/events` + 注入
  `exit_plan_mode` 的计划正文）、权限面（`permissionPresets/catalog` 映
  射线程 mode）、`@` 文件源（`fileReferences/list`）、工作区注册表 +
  目录选择器（`workspace/create|rename|delete|insertBefore`、
  `directoryPicker/list|createDirectory|pick`）、open-in-app 应用目录
  （`open-in-app/apps|open`）、逐消息与会话反馈
  （`messageFeedback/list|put|delete`、`sessionFeedback/record`）、GoalBar
  （`goals/*`，经 `/v1/threads/{id}/goal`）、工作区路径打开/定位
  （`session/openWorkspacePath|canOpenWorkspacePath|workspacePathApplications`）
  以及本地设置文档动作（`settings/openSettingsDocument`）均已接通。
- turn 运行中发出的 prompt 停到运行时线程的队列上
  （`POST /v1/threads/{id}/queue`）：队列坞从 `inbox` 投影渲染它（运行时的
  `queue.updated` 事件，实时推送），排队的 prompt 在运行中的 turn 一结束就按
  FIFO 开始、每个 turn 一条（interrupt 不清队列）。`session/updateQueue` 桥接
  队列坞的三个动作：edit（纯文本；编辑现在**保留**条目的附件——vendored
  队列坞自身对带附件的行仍禁用 Edit）、remove、steer（把条目拉进运行中的
  turn，附件随行）。熬过 `serve` 重启的队列在客户端一观看线程事件流即恢复
  （运行时加载引擎并排空——观看者可以应答该 turn 的审批）；开机不会无人
  值守地触发。排队的 turn 仍使用线程默认值，不带逐 prompt 覆盖项。
- 输入框的附件呈现是 vendored 附件插件（`client-ui-attachment`）：草稿附件
  栏（缩略图、文件卡、重试、移除）、全屏拖放遮罩与消息图片画廊，构建在已
  桥接的上传管线之上。遮罩自身的信号——文档拖放事件——在 wry 原生拖放
  下永不触发，壳的 DragDrop Enter/Leave 驱动页面上的合成
  `dragenter`/`dragleave`（`__codesmithHostDragHover`）；真正的 Drop 仍经
  宿主路径粘贴管线到达，落定后同时清除 hover（wry 在 Drop 后不发
  Leave）。上传进度结构性缺失：vendored 上传运行时的 custom-transport
  钩子（负责把上传改道到桥 origin）按契约丢弃进度回调——而环回上传几十
  毫秒内落定，进度条也不会可见地动。
- Jobs 弹窗（`client-ui-jobs` + job-controller remote）桥到运行时的后台
  任务：`job/list` 以整集 `rows` 帧服务该会话的任务名册（打开期间 2 秒轮
  询），`job/follow` 把任务 timeline 流成输出环（连续 message 增量拼接，
  其余条目各成一行 log——1 秒轮询，以终态 `status` 帧收尾），`job/kill`
  映射到 `POST /v1/tasks/{id}/cancel`（`requested` /
  `already-finished`；未知 id 为 `job/not-found`）。状态映射：
  queued/running → running、completed、failed、canceled → killed。无关联
  线程的任务不属于任何会话，名册不列出；没有进度行或字节上限可报。
- 改动审查标签页与轮尾改动文件卡（`client-ui-deliverables`）由线程的持久
  化工具条目供数：`write_file`、`edit_file` 把 unified diff 作为调用结果
  返回，`apply_patch` 返回 JSON 摘要、请求的 hunks 留在调用自身的 `patch`
  参数里（桥按文件头逐段还原），因此三者都能列出计数与对比；它的整文件
  `changes` 形式只列路径、没有对比（运行时没有写前内容可 diff）。失败或
  被拒绝的文件调用没有改动任何东西，不列出。mapper 对每个落定了文件改动
  的 turn 发 `workspace/changes` 日志事件（实时与回放一致）。桥替页面服务
  `/api/changes.summary|diff|open`（及 `/api/present.host|open`）——页面的
  fetch 钩子把页内相对 `/api/…` 路由携到桥 origin。一个 turn 内对同一路径
  的多次编辑折成一行（计数累加；对比取最后一次调用的 hunks——运行时保存
  逐调用 diff，而非 turn 首尾快照）。`changes.open` 把列出的相对路径解析到
  会话工作区，再在系统文件管理器中打开/定位（文件已不存在时 404，成功
  204）；summary 与对比路由应答 `cache-control: no-store`。`present.open`
  （声明文件开启器）按客户端的 `nativeUnavailable` 应答——CodeSmith 没有
  声明文件注册表。卡片/审查标签页的打开、定位控件由 vendored
  `client-ui-open-in-app` 填充（`deliverables.file.actions` /
  `deliverables.review.file.actions`）：`session/canOpenWorkspacePath` 为
  真、`session/workspacePathApplications` 返回空应用目录，因此控件回退到
  “定位”；`session/openWorkspacePath` 规范化后只放行 bridge cwd、已注册
  工作区或已导入线程工作区内的路径（符号链接逃逸为
  `workspace-file/outside-workspace`，文件不存在为
  `workspace-file/not-found`）。宿主还服务 open-in-app 目录与启动路由
  （`GET /open-in-app/apps`、`POST /open-in-app/open`）：目录即 vendored
  控件能标注的应用 id——操作系统文件管理器（`finder` / `explorer` /
  `filemanager`）加上按常规位置发现的编辑器、IDE、Git GUI 与终端
  （macOS 的两个 `Applications` 根与系统 Terminal；Linux/Windows 走 PATH
  可执行文件）。发现只解析本机真正持有的启动器；启动时把目录追加到 argv
  （除非某个参数带 `{path}` token，如 `--cd={path}`、
  `--working-directory={path}`），以脱离方式运行并清除名字像密钥的环境
  变量，短暂观察启动是否立即失败，并要求目标为同一工作区栅栏内已存在的
  绝对目录。会话头的“Open In…”控件与工作区行的“在文件夹显示”由此启用。
  macOS 上图标已服务：vendored 控件的图标前缀经 vendor 补丁改为指向桥的
  绝对 URL（`<img>` 加载绕过 transport 的 fetch 钩子，页内相对图标 URL
  会在资源 origin 上 404），`GET /open-in-app/icon/<id>` 回答 bundle icns
  里最大的 PNG 条目——优先主图标 `AppIcon.icns`——按 `image/png` 带
  一天缓存与 mtime:size ETag（`If-None-Match` 命中答 304；`<img>` 加载
  不带 Origin 头，路由与其它宿主路由一样放行无 Origin 请求）。Finder
  （无 bundle）、Linux/Windows 全部条目、以及只含 raw-ARGB 条目的 icns
  答 404，控件渲染自带回退字形；重命名
  或移出常规根的应用不会被发现。
- 侧栏置顶与手动排序是桌面壳自有状态，持久化在
  `~/.codesmith/desktop/workspace.json`，不写回运行时：置顶会话排在分组
  前列（最近置顶在前），手动排序保持相对顺序，从未拖动过的会话按运行时
  近期顺序跟在其后。宿主斜杠命令目录收 `/compact`（经 commands/execute
  桥到运行时的手动压缩：调用必须无参数、无附件；执行结果只代表运行时
  接受了压缩——压缩本身作为异步 turn 运行，由转写节点携带结果；会话忙
  时把运行时的冲突文案作为命令错误通知呈现）与逐会话的模式开关：
  `/mode <agent|plan|yolo|coordinator>`（必须带参数，拼错时以合法取值
  响亮失败）、`/plan [off]` 与 `/yolo` 快捷方式，以及 `/permission
  <workspace-write|danger-full-access>`（同样必须带参数）——各自
  `PATCH /v1/threads/{id}` 的 `mode` 字段，下一 turn 起生效该模式的工具
  面与审批策略。线程处于 `plan` 时 `/permission` 切换会被拒绝并提示先用
  `/plan off`，因此预设选择器不会悄悄退出计划。目录还收 `/goal
  <objective> [budget: N]` 与 `/feedback <text>`，且 `/goal`、`/plan`、
  `/permission`、`/feedback` 行携带 vendored 客户端的内建 definition id
  （`@codesmith/command-goal`、`@codesmith/plan-mode`、
  `@codesmith/permission-presets`、`@codesmith/command-feedback`），因此
  客户端的本地化拼写（`/目标`、`/计划`、`/权限`、`/反馈`）能解析到它们。
  `/model` 走 vendored 客户端自己的贡献（经 session/selectModel 的弹窗，
  已桥接）。REPL 的其余斜杠命令是 REPL 本地的，不进目录，对应 "/" 开头
  的行照常作为普通 prompt 提交。
- Models 设置页（`client-ui-settings-models`，桥接在 `host::llm` /
  `host::credentials`）：服务一个名为 `llm-pi-ai` 的设置 namespace——
  页面的通用 OpenAI 兼容编辑器正是按这个 namespace 名选布局（其他名字只
  渲染 "unknown" 提示卡）。取值 `{providers: {<route>: {apiKeyEnv?,
  displayName?, api?, baseURL?, models: [...]}}}`，底层读写 config.toml
  （`crates/config` 的 ProvidersToml + `toml_edit` 的外科式写入——注释、
  陌生键与 CLI 拥有的其它表原样保留）：route 是内置 provider slug 或
  `[[providers.custom]]` id；`apiKeyEnv` 是页面自己的 `<ROUTE>_API_KEY`
  推导（不落盘，读时重算；自托管运行时 sglang/vllm/ollama 不带，行就不
  索要 key；自定义路由仅在实际持有凭据时才带——内联 `api_key` 或
  store/env 里的密钥——无 key 的网关因此保持 create card 的
  provider-native auth 姿态，不显示缺 key 红点）；模型行映射
  `id→model`、`name→display_model`、`contextWindow→context_window`、
  `input 含 image→vision=true`。schema 信封
  带页面要探测的 `providers.<route>.api` 协议 union——只有
  `openai-completions` 一个选项（CodeSmith 的自定义网关走 openai-compat
  后端），`models[].reasoningEfforts` 刻意缺席：CodeSmith 没有按模型的
  推理档位配置，页面因此隐藏该列而不是让它编辑一个无人读取的字段。
  目录 `llm/listConfigurableProviders` 由 config 生成（16 个内置 +
  custom 条目，custom 标 declared=true），`llm/listProviders` 取自运行时
  `/v1/models`，`llm/discoverModels` 无 baseURL 时以命名路由查运行时目
  录、有 baseURL 时 reqwest 携 key GET `{base_url}/models`（OpenAI 兼容
  `{data:[…]}`，键取表单现填的或该路由已存的；这个双参数 remote 的 args
  按参数名打包，草稿在 `request` 下——`{settingsNs, request: {provider?,
  baseURL?, apiKey?}}`）。`credentials/describe|set|unset` 映射到
  keyring（`crates/secrets`，slot 即 `codesmith login` 的 provider_slot
  小写 slug），describe 只回 `{configured, source?, writable}` 绝不回
  值，set/unset 后推 `credentials/record-updated` 事件帧；`source` 为
  `file` 表示值来自凭据存储（vendored 页面称"本页保存的凭据"的写法，
  不论底层是文件还是系统 keychain），`env` 表示进程环境变量命中。
  describe 还会探测 config.toml 里该路由的内联 `api_key`（custom 条目自
  己的 key，或运行时接受的根 key），命中时回 `{configured: true,
  source: "file", writable: false}`：`source: "file"` 走页面的"已保存凭
  据"文案（留空即用已保存的 key），而不是对 config 文件里的 key 误报
  "检测到环境变量"；`writable: false` 使页面不提供它写存储也无法兑现的
  移除操作。它是运行时的第一 key 来源，页面既不能替换也不能清除它。
  revision 计数存于
  `desktop/settings.json`，过期 `expectedRevision` 以 `settings/conflict`
  拒绝。页面内建 welcome-notice 走的 `ui-settings-general` namespace 也已
  服务（默认值即当前版本号——通知默认隐藏，上游升版后出现一次、自 dismissed
  落盘）。已知限制：config.toml 每 provider 只配一个模型——写两个及以上
  模型行会被拒绝（不会静默丢）；模型行的 `maxTokens` 无 config 归宿，
  带它的写入会被拒绝而不是静默丢弃（CodeSmith 由模型的 context window
  推导输出上限），`llm/discoverModels` 也因此不再返回端点列出的
  `max_tokens`；config.toml `[providers.<route>] api_key` 里的内联密钥
  读作已配置（绿点，已保存凭据文案），但页面不能替换或移除它，运行时继
  续用内联值——在该路由上从页面输入的 key 会写入存储，却仍被 config 里
  的 key 遮住；
  页面保存的 key 落在凭据存储，且对后端有效：`codesmith serve` 构造引
  擎时按 config.toml → 进程环境 → 凭据存储解析 provider key（内置按
  provider slot，活动 custom 路由按其 `[[providers.custom]]` id）——后
  端启动后保存的 key 在重启后生效，环境变量仍会遮住页面保存的 key
  （页面自己的凭据视图先查存储再查环境变量，所以该路由仍显示为"已保存"
  ——真正发出去的是环境变量里的 key）；
  provider 改动是运行时加载期状态，需重启 serve 才生
  效；手工声明的自定义路由还需 providers.toml/扩展注册 factory 才能真正
  服务；页面首启 onboarding 弹窗依赖 `llm-deepseek` namespace（不服务）
  因而永不出现。
- 通用设置文档还服务 `locale`（Language 行跨重启持久）、`ui-settings`
  （开发者工具；默认开启，恢复 Trajectory 标签、changed-files 卡片与脚本
  HTML 预览）和 `permission`（下条的新会话默认值）。通用页自身的子面全部
  可用：外观与排版、展开后的正文字体/代码字体/终端字体、工作步骤展示与
  收起、转录视图、语言、快捷键编辑器、繁忙发送行为、性能与用量详略。
  `settings/openSettingsDocument` 在文档不存在时创建它，并用系统默认应用
  打开 `~/.codesmith/desktop/settings.json`（设置头的文档动作）。上游的
  插件/账户设置区仍缺席：CodeSmith 没有插件系统或托管账户，这些页面没有
  后端可接；这也是本设置文档的最终形态——General、Models、快捷键与
  字体/主题覆盖即是全部范围，新增插件/账户页需要先明确决定同时补上对应
  后端。
- 计划模式是完整的桌面面（`client-ui-plan`）：composer 的计划 chip 读
  `plan` 投影（`active` = 线程 mode 为 `plan`），`/plan off` 退出，提交的
  计划渲染为 turn 尾部卡片与右侧栏计划标签页。运行时的 `exit_plan_mode`
  参数 schema 为空、自己读计划文件，因此桥在重放与实时路径上把最近一次
  `write_plan_file` 的正文作为该调用的 `arguments.plan`（中途接入的实时流
  用重放快照 seed mapper）。vendored 卡片还要求 `# 标题`：模型没写标题时
  只在桥上补 `# Plan`，计划文件本身保持原样。运行时把批准的退出记为
  `status` item 而非工具 item，桥据该 item 合成 `exit_plan_mode` 调用
  （item id 即计划的 call id），计划卡与计划资源都从它取数。批准该调用
  会把线程 mode 恢复为会话进入计划前的模式——对应 REPL 的
  `plan_restore_mode`；桥在自己的 mode 写入进入 `plan` 时记下该模式。若桥
  首次看到某线程时它已在 `plan`（别的主机切换过，或桥重启后页面再次批
  准），则没有记录，回退到运行时默认的 `agent`。交接在 turn 结束事件到达
  时执行——运行时的落定保存会覆盖中途发出的 PATCH。运行时拒绝时留给计划
  chip 切换。不会自动提交实现 kickoff。已知限制：没有“请求修改”评
  审语义——审批卡仍是允许/拒绝；从未写过计划文件的 plan-mode turn 不出卡
  片。
- 权限面（`client-ui-permission-presets`）按通用 Agent 的两档预设表把 vendored
  预设选择器映射到线程 mode：`workspace-write`（线程 mode `agent`）与
  `danger-full-access`（即 CodeSmith 的 `yolo`，保留选择器的显式风险确
  认）。plan 不是预设：线程处于 `plan` 时投影为派生值 `read-only`，
  `coordinator` 投影为 `custom`；两者都不进目录、也不可选择。
  `/permission <workspace-write|danger-full-access>` 与 `/mode` 并列进入
  宿主命令目录；一次选择解析成该预设对应的 mode，并落定为同一次线程
  mode 的 `PATCH`（`permissions` 投影随后跟上），而线程处于 `plan` 时的
  选择会被拒绝并提示 `/plan off`，而不是悄悄退出计划。同一个 PATCH 携带
  `auto_approve`（`yolo`/`coordinator` 为开，其余为关）：运行时的 prompt
  组装要求这一配对，且从完全权限切到计划模式后退出计划仍须询问用户。旧
  版本遗留的“完全权限但未开 auto_approve”线程在首次 follow 或首次
  prompt 时会被修复。选择器自行本地化内建预设名（目录里 `name` 等于
  `value`，内建之外的名字按标题大小写显示），因此不再有
  `locale.preference` 往返，切换语言在下一次 catalog 拉取/重载时生效。
  通用设置行的 `permission.defaultPreset` 存在 `desktop/settings.json`，
  默认 `workspace-write`；`session/create` 会把它应用到新线程（旧版的
  `agent`/`plan`/`yolo`/`coordinator` 值在读取时归一化，其他未知值回落
  到 `workspace-write`）。
- `@` 菜单（`client-ui-reference`）经 `fileReferences/list` 列出工作区文
  件与目录：空 query 列根目录，带路径的 query 列该目录，裸片段在 5 万条上
  限的工作区索引里按上游打分搜索。隐藏项与版本控制/依赖/构建目录被跳过，
  不跟随符号链接；索引按工作区缓存，turn 落定后丢弃。`workingDirectory`
  投影（`thread.workspace`）让文件 chip 与文件链接正确解析；`session/list`
  的每一行携带它自己线程的工作区作为 `cwd`——桥把 summary 行与运行时的线程
  列表按 id 连接（每页一次环回读取，不改协议），因此早先在其他目录启动留下
  的会话仍会打开正确的工作区文件树、chip 也在正确目录解析；记录读取失败的
  行回落到桥进程所在目录。
  `sessionReferenceResolver/candidates` 返回空列表：CodeSmith 没有跨会话
  上下文注入能力，菜单只列文件。
- 侧栏工作区与空态选择器是真实的注册表行（`client-ui-workspace` +
  `client-ui-directory-picker-native`）：原生系统选择器选一个宿主目录，
  再经 `workspace/create` 采纳（绝对路径、存在且为目录；已注册路径幂等）。
  vendored 应用内浏览器包仍保留其原语——列出一个绝对宿主目录层级
  （`directoryPicker/list`：只列目录、含隐藏行、面包屑祖先、2,000 行按名
  排序的前缀与 `truncated`）与创建单个子目录（`createDirectory`）——但不再
  组合，且这是本壳的最终方案：与原生选择器一起 boot 会让两个占用者注册到
  同一组目录流槽位、同时打开两套对话框。注册表持久化在
  `~/.codesmith/desktop/workspaces.json`（原子 rename；损坏文件记录日志并
  回落到默认工作区）。桥进程 cwd 是隐式默认工作区（id
  `codesmith-default`）；`rename`、`delete`、`insertBefore` 与逐工作区会话
  顺序均持久化。会话按运行时线程自身的 `workspace` 路径分组，而非创建
  顺序：`session/create` 接受 `{workspaceId}`（或 `{cwd}`）并传给
  `/v1/threads`；被复用的空白会话仅在没有 turn 时返回。删除工作区会保留
  目录与会话日志，其会话落到客户端的 Ungrouped 桶。`directoryPicker/pick`
  经 Tauri dialog 插件打开原生系统目录选择器（取消为 `null`；无 Tauri
  句柄的纯桥宿主应答 `directory-picker/unavailable`）。取消不改变任何
  状态；选中的目录经一次 `workspace/create` 采纳，其 `replace` 基线推给
  每个打开的 `workspace/follow` 流，也就是侧栏渲染的那一行。应用内浏览器
  包仍保留 vendored 的 `list`/`createDirectory` 原语，但不 boot（见上）。
- GoalBar（`client-ui-goal`）由 `goal` 投影供数：线程引擎驻留时用实时引擎
  状态（`GET /v1/threads/{id}/goal` 应答 `loaded:true`），否则用线程日志里
  最近一次完成的 `create_goal`/`update_goal` 工具记录（读取会话不会加载
  引擎）。投影携带
  `{goal:{id, revision, objective, phase, blockedReason?, maxGoalRounds},
  roundsStarted, createdAt, updatedAt}`；`complete` 不渲染，`blocked` 带上
  运行时的阻塞原因。`goals/get` 返回实时 `GoalView` 加 `activation`
  （active 时 `armed`，否则 `disarmed`）；`goals/edit|pause|resume|clear`
  POST 运行时动作（加载引擎，先从日志播种 active/completed 目标）并推送
  刷新后的投影。运行时快照没有 goal id，桥用创建工具条目的 id 合成稳定
  身份并按条目计数版本。桌面 pause/clear 持久化在
  `~/.codesmith/desktop/goals.json`（按该身份索引）：读取路径不会加载
  引擎，但对日志回退会呈现持久化的停止状态，对已加载的引擎则就地重新
  应用；prompt 前的 reconcile 会先重放该动作，使刚加载（并从日志播种
  active 目标）的引擎不能在轮次收尾时继续驱动。目标条本身不创建，但宿主
  `/goal <objective> [budget: N]` 命令会创建（直接 POST 运行时的 `edit`
  动作：没有目标时创建，目标不变时保留进度，并从 `budget: N` 后缀采纳
  可选 token 预算——与 REPL 相同的后缀；输入字面为 pause/resume/clear 时
  拒绝创建并指向目标条）。变更强制客户端的比较交换（`ref {id, revision}`）：
  过期或缺失的 ref 答 `goal/stale-revision` 或 `gateway/arguments-invalid`
  ——即上游 GOAL_STALE_REVISION 防线——且合成的 revision 是日志条目计数加
  持久化 overlay 计数，ref 跨桥重启依然有效。已知限制：经 `/goal` 创建的
  目标没有日志条目，
  引擎逐出后完全无法恢复；引擎逐出后由运行时自行启动的排队
  turn 若中间没有桌面 prompt，可能抢在 pause 重放前多跑一轮。客户端可选的
  `maxGoalRounds` 编辑只接受运行时固定上限（25），其他值以
  `goal/rejected` 拒绝，不会静默忽略。
- 文档预览与改动文件的打开控件（`client-ui-open-in-app` +
  `client-ui-file-actions`）经既有工作区文件面桥接：
  `session/canOpenWorkspacePath` 为真，`session/workspacePathApplications`
  返回空应用目录（本桌面不注册逐路径应用处理器，控件回退到“定位”），
  `session/openWorkspacePath` 规范化路径后要求其位于 bridge cwd、已注册
  工作区或已导入线程工作区内才打开/定位。File Actions 的 Download 与
  Copy image 走 `workspaceFiles/readBytes` 与浏览器剪贴板；Save as 保持
  隐藏（需要本壳未组合的原生保存对话框），`present.open` 仍为
  `nativeUnavailable`。会话头的“Open In…”控件与工作区行的“在文件夹显示”
  经宿主 open-in-app 目录启动已安装应用（文件管理器排第一）；逐路径的
  `workspacePathApplications` 目录仍为空，因此文件/文档控件渲染“定位”
  动作而非应用菜单。
- 逐消息反馈与 `/feedback` 对话框（`client-ui-message-feedback`）由
  `~/.codesmith/desktop/feedback.json` 的桌面本地 compare-and-set sidecar
  支撑。`messageFeedback/list|put|delete` 校验会话存在、目标为已完成的
  assistant item，并执行 UUID `ifVersion` 比较、1,024 字节 note 上限与固定
  分类词表；`sessionFeedback/record` 追加会话级记录。Like/Dislike 控件与
  对话框从每次变更的回复更新，`/feedback <text>` 进入宿主命令目录并携带
  `@codesmith/command-feedback` definition id（客户端本地化的 `/反馈`
  别名因此可解析）；vendored 插件只给这一条宿主行的裸调用挂上对话框
  装饰，不会注册第二个菜单贡献。对话框文案改为“反馈只保存在本机”（上游
  原文声称会上传当前对话日志；本壳不上传任何内容）。已知限制：vendored
  面没有实时事件钩子，因此没有跨客户端反馈推送（CAS 回复会让输掉竞争的
  客户端自行对齐，重载后重读 sidecar）；删除会话日志会在 sidecar 留下
  孤儿行。CAS list/put/delete 契约已用 fake runtime 端到端覆盖
  （`host::tests::feedback_dispatch_enforces_the_sidecar_contract`）。
- 控制流投影还携带 `todos`（最近一次成功的整表 `todo`/`checklist_write`
  调用，工具 `item.started` 时实时推送；单项的
  `todo_add`/`todo_update`/`checklist_add`/`checklist_update` 从结果
  metadata 折叠其快照）、`plan`、`permissions`、
  `workingDirectory`、`goal`（跟随会话的快照与实时 goal 更新；侧栏控制
  基线省略它，因此不会读到别的会话的目标）与 `turnOutline`（每个已开始
  turn 的 seq 与截断的 prompt/response 预览）。`contextBreakdown` 与
  `userQuestions` 仍缺席：运行时没有记录逐请求的 system/tools/message
  组成，问题请求也没有 tool-call id 或取消路由。
- 右侧栏终端标签页（`client-ui-sidebar-terminal`，经 `terminal/*`
  remote，`host::terminal`）是桥自己 spawn 的真实用户 shell：
  `terminal/create` 在会话工作区开一个 PTY（`portable-pty`），环境经
  清洗（`TERM=xterm-256color`、`CODESMITH_SESSION_ID`，疑似凭据的变量
  剔除）——每会话上限 8 个，关闭过的身份不可重建，重复 close 幂等。
  `terminal/shells` 以环境默认 shell 打头探测上游候选清单；
  `terminal/environment` 报会话目录与镜像的上游限额（500×200、单条
  输入 64 KiB、每 follower 队列 2 MiB、历史 1000 行）。
  `terminal/attach` 是唯一的双工流：输入走 mux 上行（write/resize 按
  每次附着从 1 连续编号、按序 ack），输入控制权只属于最新附着（被
  降级的附着 ack `read-only`，退出后 `not-running`），每个附着以整屏
  snapshot 开场——一个 vt100 仿真器镜像 PTY，把可见屏序列化为可回放
  的转义序列（即上游 headless xterm 的角色），其上方是宿主自维护的
  剥离转义后的纯文本行环，补上 vt100 视口走不了的滚动历史。输出
  sequence 自 snapshot 起严格连续；follower 队列超过 2 MiB 时该流
  失败，重连重新 snapshot 恢复（上游的恢复模型）。`terminal/retain`
  确认窗口持有（本宿主不做无人值守回收）。`terminal/close` 先向 shell
  进程组发 TERM、宽限期后升级 KILL、等输出排空，然后结束各 follower
  的流。这些是用户的终端，不是模型的——`allow_shell`（模型的 shell
  工具开关）不约束它们。已知限制：终端随桥进程生死（上游 host 进程
  同样如此）；历史行环是剥离文本（可见屏以上没有颜色，且可能重复仍
  可见的行）；`attach` 不携带 `inputWindow`（mux 上行 inbox 按帧数计
  而非字节计——wire 类型对这类载体省略该字段）；`shellPath` 接受任意
  存在的可执行文件而非仅发现清单（本宿主没有沙箱化 subprocess
  provider，页面源门就是边界）；shell 死后仍握着 PTY 的孙进程在排空
  超时后失去的是输出流，不是生命。
- 上游独有概念不仿真：账户/余额与账户设置区（`account/*` 返回未登录形态
  或 null）、插件管理器/市场与设置里的插件清单（Cordis/JS 插件体系在
  CodeSmith 没有对应物）、逐会话 agent presets（`agentPresets/list` 返回
  空名册；CodeSmith 在启动时选择配置 preset，而不是逐会话的 agent 组合）、
  右侧栏浏览器标签页（没有用户可见的浏览器会话；终端标签页**已**桥接
  ——见其条目）、子代理子
  会话视图（CodeSmith 的 subagent 是进程内执行，不是带日志的独立 thread）
  以及插件条目定制。对应插件不进 boot roster
  或返回空态。workflow 生命周期走 extension event、没有写进会话日志，
  因此 workflow-run 面板也没有数据源。（Jobs 弹窗、附件呈现、改动审查
  标签页、右侧栏文件/预览标签页、右侧栏终端标签页、计划页、权限面、
  `@` 文件源、工作区
  选择器/注册表、GoalBar、打开/定位/文件动作与逐消息反馈面**已**桥接
  ——见各条。）
- 右侧栏文件树与文档预览（`client-ui-sidebar-files` +
  `client-ui-sidebar-documentpreview`，经 `workspaceFiles` remote，
  `host::files`）直接读会话工作区：`workspaceFileScope` 从线程记录解析
  根，`list` 服务单层目录（工作区路径词汇、词法上锁死在根内），`read`
  分页读文本，`stat` 给新鲜度令牌（`mtime:size`），`changes` 是 1 秒
  stat 轮询、推上游的 `ready`/`change` 帧。`readBytes` 以
  multipart/form-data 应答——envelope 顶层带附件清单，其 value 里
  `data: null`，字节作为带 `filename` 的二进制文件 part 随行（没有
  filename 时 Fetch 解析器会按 UTF-8 文本处理而失败），由页面 carrier
  植回（客户端的严格 codec 只接受真 `Uint8Array`）。渲染器为 vendored
  纯 web 集合（markdown、代码、文本、图片、PDF、Excel/CSV）；OFFICE
  渲染器的转换 RPC（`officeToPdf/render`）外呼本机 LibreOffice
  `soffice`（PATH、macOS bundle 或 `CODESMITH_DESKTOP_SOFFICE`），每次
  转换用隔离的 soffice profile——源文件经工作区围栏暂存并复核版本
  令牌（变更答 `source-changed`），PDF 经校验（`%PDF-`/`%%EOF`、大小
  上限）后走 `readBytes` 同款的 multipart 字节附件应答；引擎缺失时答
  线上的 `unavailable` reason，面板渲染本地化的不可用态而非假装成功
  （`officeToPdf/generation` 为进程级 UUID；桥侧无结果缓存——客户端按
  generation+version 缓存）。读取拒绝末段
  符号链接（上游自身的门），`baseFile` 相对解析未实现（预览按绝对路径
  寻址文件）。
- 附件上传已端到端桥接。输入框的上传先暂存在桥里（`fileUploads/upload`
  与原始字节路由 `api/session/uploadFileBinary`，由页面安装的
  `__CODESMITH_FILE_UPLOAD__` 钩子改道到桥的 origin），`session/prompt`
  把暂存回执解析为 runtime turn API 的内联 `attachments[]`。图片以原生
  image 块到达模型（魔数校验、尺寸从头解析）；其余文件经 REPL 同一套
  摄取管道折进消息（内联文本，或模型可按需 `read_file` 的持久存储路径
  引用行）。已结算消息保留缩略图/文件条（runtime 把每个附件记录在 user
  item 上；`session/attachment` 从持久存储取回字节，重启后历史仍可渲染）。
  从访达/桌面拖进输入框走 host-path 桥：原生 drag-drop 开启时 DOM 收不到
  文件 drop（wry 自己认领 `performDragOperation`），窗口的 DragDrop 事件
  把每个路径描述给页面（`__codesmithHostDrop`），页面把它重放进输入框
  自己的 paste 摄取管道并让 `__CODESMITH_HOST_PATHS__` 应答——非图片文件
  与目录变成 `@path` 引用条，限额内的图片（魔数嗅探 png/jpeg/gif/webp）
  取一次性暂存字节（`api/host/dropBytes`）走常规上传管线。限额与缺口：
  单文件 10MB、每条消息 8 个、音频/视频拒绝（在上传时以业务错误呈现）；
  回执是内存态——桌面重启丢弃已暂存未发送的上传，需重新附上；无上传进度
  条；无 EXIF 方向修正或图片归一化（字节原样传输）；超出限额、或字节
  未能送达的拖入图片退化为路径引用而非上传；拖放需要打开且空闲的
  输入框，且没有拖拽悬停高亮；回形针选择器与真实粘贴仍无路径、照旧
  上传。
- 任务页：automations 只接受两种 RRULE 形态（`FREQ=HOURLY` 带
  INTERVAL/BYDAY，`FREQ=WEEKLY` 带 BYDAY/BYHOUR/BYMINUTE，按机器本地
  时区求值）。任务页的每日/每周/整小时间隔/cron 形态会编译过去；一次性
  定时、不足一小时的间隔、异时区、其余 cron 形态在创建/编辑时响亮失败，
  而不是静默改期。"让模型创建"只打开会话——模型没有注册日程的工具，
  请用手动表单。
- 工具调用的输出增量不流式——vendored 卡片本身也不消费实时输出
  （运行中的卡片显示流光动画，由 `tool/result` 落定），与上游交互一
  致。工具名在桥的事件映射处规范化为 vendored 卡片词汇（`exec_shell`
  → bash 卡、`read_file` → read 卡……）；运行时记录保留 CodeSmith 真
  名。工具条目把参数记入附加的 `metadata.arguments`（被结果自身的元
  数据替换时保留），因此回放的工具调用能渲染参数；无参数记录的回放
  （该字段存在之前的外来 HTTP 客户端回合）回退为 `{}`。
  `toolResults/watch`（轨迹视图的按需取结果）按 seq 回答已落定的
  `tool/result` 线事件——无状态地从线程记录重放推导；映射不到的 seq
  回 `{available:false}` 并挂起。
- Token 计量端到端打通。运行时按"回合"上报 usage；最后一个 agent 条
  目的完成载荷携带它（运行时把这次发射推迟到回合收尾——usage 届时已
  知；回合中途落定则在下一个引擎事件前发射，保持日志顺序），桥再把
  客户端的 usage 样本挂到该条目的 `assistant/message` 上
  （`inputTokens` = 未命中份额、`cacheReadTokens` = 命中份额、
  `totalTokens` = 输入 + 输出），消息 source 带座位上真实的
  (provider, model)——客户端的回合折叠把样本聚成轮尾用量胶囊，实时
  与回放一致（回放把 `turns[].usage` 盖在该轮最后一个落定的 agent 消
  息上）。`sessionStats` 与 `tokenUsage` 投影（输入框的活动/用量
  pill）从持久化记录折出：回合数、步数、LLM/工具耗时、解码 token；
  TTFT 无法从持久化记录分离，投影不报，解码时长用 agent 条目完整跨
  度（偏保守的速度读数）。上下文表的分母由 `contextPressure` 投影里
  的 `contextWindow` 提供，来自 `GET /v1/models` 的逐模型
  `context_window` 估计（运行时估不出时省略——表随之保持隐藏）；
  CodeSmith 不上报缓存写桶，该行永不出现。
- 用户问题没有 HTTP 取消路由，未作答的问题 waterfall 等运行时自身的
  截止时间；问题请求不带工具调用 id，输入框的问题卡不渲染倒计时。
- `$events` waterfall 遵循上游 gateway 契约：每条流有自己的
  `clientId`，仍待决的请求会重放给新连接的流，迟到/重复的
  `$events/result` 是幂等 no-op（`delivered:false`，含运行时 404）。决定
  送达后紧跟该请求的 cancel 帧——各个客户端上答题卡的退役靠的就是这一
  帧。瀑布只在首次注册时推送：follow 流与后台 watcher 读的是同一条运行时
  事件尾，二次推送会让客户端的瀑布链双重派发（审批卡活过自己的决定，
  要再点一次才消失）。检测不依赖客户端正在 follow 该会话：每次 prompt
  都为会话挂一个
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
  放行）。上行 item 路由到对应的流（侧栏终端的 attach 是唯一的读取方；
  handler 丢弃上行接收端的流照上游未读 inbox 的方式把 item 吞掉），上行
  inbox 打满时该流以 `gateway/uplink-overflow` 失败。mux 不发 ping 心跳
  （环回：页面消失即关闭其
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
  模型工具回合（真实工具调用之上的活审批卡）因此仍未点验。脚本化 mock
  在 wire 层补上了这个缺口：“写文件” prompt 让它流式返回结构化的
  `write_file` 工具调用，审批回合在两层都点验过——桥协议（`$events`
  瀑布到达、`$events/result` 的 allow/deny 送抵 `/v1/approvals/{id}`、
  journal 记下两笔决定、放行的调用写盘而拒绝的那次保持字节不变）和
  活体 UI（审批卡由瀑布渲染、允许一次执行写入、拒绝把否决回喂给模型，
  任一决定后卡片与侧栏待审批徽标都退役）。
- 去品牌改写是文本级的（审计说明见 `vendor_client.py` 模块注释）：未来
  上游若出现依赖品牌字符串长度的偏移量，脚本不会发现——
  `check_brand.py` 管品牌命中，升级上游 commit 时需人工复核偏移。
- 与上游客户端的像素一致性是人工截图比对
  （`desktop/screenshots/baseline/`），未自动化。我方四张基线（空态/
  fixture × 明暗，1200×800）已捕获；上游对照待人工——2026-11-07 的尝试
  被 Electron 二进制下载失败、上游 host CLI 未构建、本机窗口捕获被系统
  涂黑三重阻断（证据见 `desktop/screenshots/baseline/README.md`）。

## 对齐路线图

桌面端持续向上游客户端的设计收敛：共有功能沿用上游设计、机械换牌
（Logo 与产品命名保持 CodeSmith）。本表是这一收敛的持久跟踪面——
每次 vendor 升级与每个落地的切片都要同步更新；各功能的细节见上文
各条。

| 面板/能力 | 现状 | 下一步 | 决策 |
| --- | --- | --- | --- |
| 侧栏终端标签 | 已桥接 | — | 已落地 |
| workflow 运行面板 | 已 vendor、无数据 | 运行时侧把 workflow 生命周期事件写入 journal | 候选切片 |
| 子代理 / 团队看板 | 未 vendor | in-process 子代理不是线程，需运行时侧设计 | 待设计 |
| 插件管理器 / 市场 | 不模拟 | 对接 CodeSmith extensions + Rhai mods | **待产品决策** |
| Agent 预设 | 不模拟 | 对接启动预设体系 | **待产品决策** |
| Office 文档预览 | 已桥接（外呼本机 `soffice`） | — | 已落地 |
| open-in-app 图标 | macOS icns → PNG 已服务 | Linux/Windows 保持字形 | 已落地 |
| goal 持久 CAS | 已落地（变更 ref 防线） | — | 已落地 |
| 像素基线 · 上传进度 | 上文已知限制各条 | 逐条清偿 | 排队 |
| 自动更新 · 欢迎页 · 托盘 | 无 | 桌面壳切片（Tauri updater 等） | 后续可选 |
| 账号 / 平台 · 侧栏浏览器标签 | 不模拟 | — | 不跟进（上游品牌基建；Electron 嵌套 webview 无 wry 对等） |
| vendor 刷新 | pin `8bd07339`，与本地 checkout 持平 | 上游演进后重跑 vendor 管线并修复 wire 漂移 | 常设机制 |
