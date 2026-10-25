# 沙箱威胁模型

CodeSmith 执行由 AI 推理派生的 shell 命令。沙箱模块限制这些命令对宿主
系统能做的事。本文档描述每个平台的沙箱实际强制执行什么、哪些是尽力
而为，以及哪些明确超出范围。

## 平台概览

| 机制 | 平台 | 类型 | 状态 |
|---|---|---|---|
| Seatbelt | macOS | 强制访问控制 | 已强制执行 |
| Landlock | Linux | 文件系统访问控制 | 已强制执行（内核 5.13+，仅文件系统） |
| seccomp BPF | Linux | 系统调用过滤 | 已移除 |
| 进程加固 | Linux | 内核 prctl / rlimit | 已移除 |
| Bubblewrap (bwrap) | Linux | 命名空间隔离 | 安装即强制执行 |
| Windows 沙箱 | Windows | — | 未实现（存根）；失败关闭拒绝 |

## 威胁模型：每一层针对什么

### 1. 进程加固（仅 Linux）——已移除

prctl/rlimit 进程加固模块（原 `process_hardening.rs`：`PR_SET_DUMPABLE=0`
阻止 ptrace、`PR_SET_NO_NEW_PRIVS=1` 阻止提权、`RLIMIT_CORE=0` 阻止核心
转储）已随同批死代码清理从代码库删除：删除时它没有任何调用点，上述加固
从不作用于运行中的进程。Linux 上真正的防线是 Landlock + bwrap（见第 2 节
与第 4 节）。

### 2. Landlock（Linux，内核 5.13+）

**状态：** 已强制执行。`prepare_landlock` 在父进程侧构建规则集（把
策略翻译为 path-beneath 规则）并随 `ExecEnv` 携带；每个
`std::process::Command` 派发点在 fork 与 exec 之间应用它
（`install_sandbox_preexec`，一个执行 `PR_SET_NO_NEW_PRIVS` +
`landlock_restrict_self` 的 `pre_exec` 闭包）。`enforces_isolation()`
返回 true，`decide_sandbox` 将 Landlock 选择标记为强制执行。

**规则翻译：**
- `/` 获得读 + 执行（所有策略都授予全盘读；子进程可执行自身二进制
  与加载器）
- `/dev/null` 获得写（shell 重定向可用）
- 每个策略可写根获得读 + 写
- 处理：`EXECUTE`、`READ_FILE`、`READ_DIR`、`WRITE_FILE`、
  `REMOVE_DIR`、`REMOVE_FILE`、`MAKE_DIR`、`MAKE_REG`、`MAKE_SYM`、
  `TRUNCATE`

**防御什么：**
- 写入系统目录（`/usr`、`/bin`、`/lib`、`/etc`）
- 在可写根之外创建、删除或截断文件（只读策略没有任何可写根）

**不防御什么（已知局限）：**
- 网络访问——此处的 Landlock 只处理文件系统权限；拒绝网络的策略
  （如 `read-only`）在该后端下仍允许网络。bwrap（安装即自动选用）
  会 unshare 网络命名空间并强制执行拒绝。
- 可写根内的只读子路径——Landlock 规则只会增加权限，受保护的控制面
  路径（`.codesmith`、`AGENTS.md` 等）在工作区可写根内保持可写。
  Seatbelt 与 bwrap 会强制执行它们。
- PTY（`tty: true`）派发——PTY 派发路径没有 pre-exec 钩子，因此
  Landlock 约束的 TTY 请求会被拒绝（`SANDBOX_UNAVAILABLE`），并给出
  补救建议（不带 tty 重跑或安装 bubblewrap），而不是无约束地派发。
- 进程检查
- 读取已被映射的文件（Landlock 在 `open()` 时生效）

**检测：** `detect_denial()` 检查 stderr 中的 `Permission denied`、
`Operation not permitted`、`EACCES`、`EPERM`。

### 3. seccomp BPF（仅 Linux）——已移除

手写的 seccomp BPF 过滤器模块已从代码库删除：删除时它没有任何
调用点（Landlock + bwrap 才是 Linux 上真正的防线），其手工维护的
系统调用白名单也仅覆盖 x86_64。仅存的部分是 `landlock.rs` 中
`detect_denial()` 里的 SIGSYS / 退出码 31 检测启发式（见第 2 节）。

### 4. Bubblewrap / bwrap（Linux，可选）

**何时运行：** 只要 `/usr/bin/bwrap` 存在就会运行——bwrap 赢得 Linux
后端选择，因为其命名空间隔离额外拒绝网络（此处的 Landlock 仅文件
系统），并把可写根内的受保护控制面子路径保持只读，而 Landlock 的
规则并集语义表达不了这一点。旧版顶层 `prefer_bwrap` 键与
`[sandbox] prefer_bwrap` 因此已废弃并被忽略。作为子命令的外层包装
运行。

**做什么：**
- 用 `--unshare-all` 创建新的挂载命名空间
- 将整个根文件系统以只读方式绑定挂载
- 以读写方式绑定挂载工作区目录
- 用 `--chdir` 切换到工作区

**防御什么：**
- 工作区之外的任何文件系统写入（在命名空间级别强制执行，
  而不仅是文件系统访问）
- 意外修改系统文件

**不防御什么：**
- 策略授予网络时的网络访问——`--unshare-all` 隐含 `--unshare-net`，
  只有当策略授予网络时才显式追加 `--share-net`，因此 bwrap 下的
  网络由策略把关
- 进程检查
- 内存攻击

**安装：** 用户必须自行安装 bubblewrap：
- Ubuntu/Debian: `apt install bubblewrap`
- Fedora: `dnf install bubblewrap`
- Arch: `pacman -S bubblewrap`

CodeSmith 不会内置（vendor）bwrap。

**回退：** 若未安装 bwrap，Landlock（内核 5.13+）是强制执行后端——
仅文件系统，见上面的已知局限。若两者都不存在，决策默认失败关闭
（见[失败关闭语义](#失败关闭语义)）；`[sandbox] fail_if_unavailable
= false`（或 `CODESMITH_SANDBOX_FAIL_IF_UNAVAILABLE=false`）是显式的
降级选择，命令将无约束运行。

### 5. Seatbelt（macOS）

**何时运行：** 通过 `sandbox-exec` 包装命令应用。Seatbelt 配置文件
基于 `SandboxPolicy` 动态生成。

**做什么：**
- 依据策略配置文件限制文件系统访问
- 可限制网络访问（当 `network_access: false` 时）

**防御什么：**
- 读/写允许路径之外的文件
- 网络连接（已配置时）

**不防御什么：**
- 进程检查（Seatbelt 不阻止 ptrace）
- 系统调用级攻击

**检测：** `detect_denial()` 检查 stderr 中的 `Operation not
permitted`、`deny(` 和 `Sandbox: `。裸二进制名 `sandbox-exec` 被刻意
排除——它会对任何提及该二进制名的 stderr 行（如 usage 消息）误报；
`was_denied()` 已先确认命令确实运行在 sandbox-exec 之下，EPERM
（`Operation not permitted`）由此保持可佐证。（`file-write` 和
`network` 只出现在人类可读的拒绝消息中，不用于检测。）

### 6. Windows——未实现

CodeSmith 不提供进程内 Windows 沙箱。`windows.rs` 是一个存根：
`is_available()` 返回 false，不存在任何 Job Object、内存上限或受限
令牌代码。**Windows 上沙箱策略不被强制执行**：要求约束的策略会被
决策为 `SANDBOX_UNAVAILABLE`，且在默认失败关闭姿态下命令被拒绝
（可诊断、指明替代方案），而非静默无约束运行。Windows 当前的逃生
通道：在配置中禁用沙箱（`[sandbox] enabled = false`）、显式设置
`fail_if_unavailable = false`（降级：命令无约束运行）、或使用无约束
模式。未来的 Windows 支持必须通过专用辅助进程提供带 Job Object 和
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的进程树遏制，并且在文件系统、
网络、注册表或 AppContainer 隔离真正实现并单独测试之前，不得宣称
这些保证。

## 失败关闭语义

要求约束的沙箱策略（`read-only`、`workspace-write`）是一个安全声明
而不是愿望：当平台上没有任何后端能强制执行它时，CodeSmith 拒绝运行
该命令。拒绝是可诊断的——理由携带 `SANDBOX_UNAVAILABLE` 标记、平台
缺什么、以及替代方案（安装可强制执行的后端；通过 `[sandbox] enabled
= false` 或 `fail_if_unavailable = false` 显式退出；或使用无约束
模式）。显式的配置选择（`enabled = false`、`excluded_commands`）会
记录在每个结果上，但绝不会在强制执行后端缺席时静默替代为无约束
运行。

`fail_if_unavailable` 默认为 `true`。设置为 `false` 恢复旧的
无约束运行回退——拒绝理由仍保留在结果记录上
（`sandbox_unavailable_reason`、`sandbox_effective = false`），降级
运行绝不会被误报为已沙箱化。

## 一次性沙箱升级走审批

严格更宽的一次性沙箱策略（例如以 `danger-full-access` 重试被阻断的
命令）只能通过审批门生效：宿主发送携带该策略的
`ApprovalDecision::RetryWithPolicy`，回合循环经 dispatcher 的
`sandbox_override` 接线点重新派发工具。该接线点拒绝任何不是当前
基线**严格更宽**的策略（`ReadOnly < WorkspaceWrite < DangerFullAccess`；
增加可写根/网络或放开临时目录排除是放宽；没有任何策略比
`DangerFullAccess` 更宽；`ExternalSandbox` 不可比较），因此更宽的
策略无法经由其他路径生效，"升级"也不可能变窄。

## 纵深防御

Linux 上两个本地后端都强制执行——bwrap（挂载命名空间）在已安装时，
Landlock（文件系统规则，内核 5.13+）作为后备。prctl/rlimit 进程
加固模块与 seccomp 过滤器均已从代码库移除：

```
bwrap (namespaces + network + subpaths)  ← 优先，已安装时
Landlock (filesystem-only)               ← 强制执行后备，内核 5.13+
```

Linux 上两者都不可用——或 Windows 上不存在辅助程序——时，
`decide_sandbox` 报告不可用，命令在默认失败关闭姿态下被拒绝。
在 macOS 上，强制执行层是 Seatbelt。

## 配置

`~/.codesmith/config.toml` 中的相关配置键：

```toml
# Sandbox policy mode
sandbox_mode = "workspace-write"  # read-only | workspace-write | danger-full-access | external-sandbox

# Linux bubblewrap — DEPRECATED: bwrap is now selected whenever the
# `bubblewrap` package is installed; this key is parsed for compatibility
# and ignored.
# prefer_bwrap = false

# Structured runtime policy. These keys are optional and layer on top of
# the legacy flat keys above.
[sandbox]
enabled = true
fail_if_unavailable = true        # 默认：要求约束的策略无法强制执行时拒绝（SANDBOX_UNAVAILABLE）；false = 无约束运行（降级）
enabled_platforms = ["macos", "linux"]
excluded_commands = []            # program names or command prefixes
auto_allow_bash_if_sandboxed = true
# prefer_bwrap — deprecated & ignored (see above)

[sandbox.filesystem]
mode = "workspace-write"          # read-only | workspace-write | danger-full-access | external-sandbox
writable_roots = []
allow_read = []
deny_read = []
allow_write = []
deny_write = []
exclude_tmpdir = true            # 临时目录默认排除写入；需要时显式设为 false 放开
exclude_slash_tmp = true

[sandbox.network]
enabled = true
allow_managed_domains_only = false # local OS sandboxes cannot enforce host allow-lists
allow = []
deny = []

# External sandbox backend
sandbox_backend = "none"          # "none" or "opensandbox"
sandbox_url = "http://localhost:8080"
sandbox_api_key = "YOUR_API_KEY"
```

环境变量覆盖：

- `CODESMITH_SANDBOX_MODE` → `sandbox_mode`
- `CODESMITH_PREFER_BWRAP=true` → `prefer_bwrap` (deprecated & ignored — bwrap
  is selected automatically whenever installed)
- `CODESMITH_SANDBOX_BACKEND` → `sandbox_backend`
- `CODESMITH_SANDBOX_URL` → `sandbox_url`
- `CODESMITH_SANDBOX_API_KEY` → `sandbox_api_key`
- `CODESMITH_SANDBOX_ENABLED=true|false` → `[sandbox].enabled`
- `CODESMITH_SANDBOX_FAIL_IF_UNAVAILABLE=true|false` → `[sandbox].fail_if_unavailable`
- `CODESMITH_SANDBOX_ENABLED_PLATFORMS=macos,linux` → `[sandbox].enabled_platforms`
- `CODESMITH_SANDBOX_EXCLUDED_COMMANDS=cmd1,cmd2` → `[sandbox].excluded_commands`
- `CODESMITH_AUTO_ALLOW_BASH_IF_SANDBOXED=true|false` → `[sandbox].auto_allow_bash_if_sandboxed`

## 检测沙箱拒绝

当命令失败时，沙箱管理器会检查拒绝模式。Shell 工具元数据还会报告
是否请求了沙箱以及沙箱是否实际生效（`sandbox_requested`、
`sandbox_effective`、`sandbox_backend`、`sandbox_unavailable_reason`、
`sandbox_fallback_allowed`、`sandbox_excluded_command` 和
`sandbox_fail_closed`）。

| 平台 | 拒绝机制 | 退出码 | Stderr 模式 |
|---|---|---|---|
| macOS Seatbelt | sandbox-exec 违规 | 非零 | `Operation not permitted`、`deny(`、`Sandbox: ` |
| Linux Landlock | EACCES / EPERM | 非零 | `Permission denied`、`Operation not permitted`、`EACCES`、`EPERM` |
| Linux SIGSYS 启发式 | seccomp 时代遗留的检测（Linux 共用路径） | 31 | `Bad system call`、`bad system call`、`SIGSYS`、`seccomp` |
| Linux bwrap | 挂载/命名空间失败 | 非零 | 不定 |
| Windows | 拒绝访问 / 特权 | 非零 | `Access is denied`、`ERROR_PRIVILEGE_NOT_HELD` |

`SandboxManager` 上的 `was_denied()` 方法聚合所有平台特定的检查。
`denial_message()` 方法返回人类可读的解释。

## 局限

### 沙箱不防御什么

- **网络攻击** — macOS Seatbelt 可以在本地阻断网络，Linux bwrap 按策略
  把关网络（策略未授予网络即 `--unshare-net`）。Windows 上没有任何
  强制执行。因此 `[sandbox.network].allow_managed_domains_only = true`
  在本地 OS 沙箱下按拒绝网络处理，并在外部沙箱请求中透传，交由后端
  强制执行。
- **Git hook/fsmonitor 执行** — 当没有提供显式索引的 `GIT_CONFIG_*`
  环境时，CodeSmith 会向 shell git 配置注入空的 `core.fsmonitor` 和
  `core.hooksPath` 值，防止工作区 git 配置在沙箱化工具调用期间启动
  宿主侧辅助程序。
- **内存攻击** — 没有任何平台能阻止子进程读取自己的内存或利用内存
  破坏漏洞
- **时序侧信道** — 没有任何本地沙箱层能防御基于时序的信息泄露
- **资源耗尽** — 没有任何本地后端限制 CPU、内存、文件描述符或磁盘 I/O
- **内核漏洞** — 若内核本身存在漏洞，沙箱无法阻止利用（这适用于
  所有平台）
- **供应链** — 若子进程下载并执行不可信代码，沙箱限制该代码能做的
  事，但不阻止下载

### 平台特定缺口

- **Linux：** 未安装 bubblewrap 时，Landlock 仅强制执行文件系统访问
  ——网络不受限制，受保护的控制面子路径在可写根内保持可写；PTY
  派发被拒绝而非无约束运行。两者都没有时，要求沙箱的命令被拒绝
  （失败关闭默认）。主机允许列表网络策略不在本地强制执行。
- **macOS：** Seatbelt 配置文件在运行时生成。配置不当的配置文件
  可能过于宽松。
- **Windows：** 未实现任何沙箱；要求沙箱的命令在默认失败关闭姿态下
  被拒绝（见第 6 节）。

## 相关

- `crates/cli/src/sandbox/` — 宿主侧策略准备、运行时接线、外部后端（`opensandbox.rs`）
- `crates/agent-runtime/src/sandbox/` — 强制执行辅助程序（`seatbelt.rs`、`landlock.rs`、`windows.rs`、`bwrap.rs`；由 `crates/cli/src/sandbox/` 垫片重导出）
- `crates/config/src/lib.rs` — 配置键
- `crates/tool-impls/src/tools/diagnostics.rs` — `diagnostics` 工具报告
  `sandbox_available`、`sandbox_type`、`bwrap_available`、
  `cgroup_version`
- `config.example.toml` — 带注释的配置参考
