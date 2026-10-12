# 沙箱威胁模型

CodeSmith 执行由 AI 推理派生的 shell 命令。沙箱模块限制这些命令对宿主
系统能做的事。本文档描述每个平台的沙箱实际强制执行什么、哪些是尽力
而为，以及哪些明确超出范围。

## 平台概览

| 机制 | 平台 | 类型 | 状态 |
|---|---|---|---|
| Seatbelt | macOS | 强制访问控制 | 已强制执行 |
| Landlock | Linux | 文件系统访问控制 | 未强制执行（仅检测） |
| seccomp BPF | Linux | 系统调用过滤 | 已移除 |
| 进程加固 | Linux | 内核 prctl / rlimit | 存在但未被调用 |
| Bubblewrap (bwrap) | Linux | 命名空间隔离 | 安装即强制执行 |
| Windows 沙箱 | Windows | — | 未实现（存根） |

## 威胁模型：每一层针对什么

### 1. 进程加固（仅 Linux）

**状态：** 已在 `process_hardening.rs` 中实现，但未接入启动流程——
`apply_process_hardening()` 没有任何调用点，以下加固从不会作用于
运行中的进程。

**该模块做什么（被调用时）：**

- `PR_SET_DUMPABLE=0` — 阻止 ptrace，使 `/proc/<pid>/` 归 root 所有
- `PR_SET_NO_NEW_PRIVS=1` — 不可逆；任何子进程都无法再获得特权
- `RLIMIT_CORE=0` — 不产生核心转储，敏感数据永不落盘

**防御什么：**
- 通过 ptrace/strace/gdb 进行的进程检查
- 通过 setuid/setgid/fscaps 的提权
- 泄露 API 密钥、令牌、提示内容的核心转储

**不防御什么：**
- 被攻陷的子进程读取其父进程的 `/proc/<pid>/mem`（已由
  `PR_SET_DUMPABLE=0` 使 `/proc/<pid>/` 归 root 所有而阻断）
- 绕过 prctl 的内核漏洞利用

### 2. Landlock（Linux，内核 5.13+）

**状态：** 未强制执行。规则集应用代码存在，但未接入子进程强制执行
路径：`prepare_landlock` 是有文档记录的透传，返回 `SandboxType::None`；
`LinuxLandlock.enforces_isolation()` 返回 false；`decide_sandbox` 会把
Landlock 选择报告为不可用。只有下面的拒绝检测模式是实际生效的。

**规则集代码限制什么：**
- 将文件系统访问限制在一个路径白名单内
- 处理：`EXECUTE`、`READ_FILE`、`READ_DIR`、`WRITE_FILE`、
  `REMOVE_DIR`、`REMOVE_FILE`、`MAKE_DIR`、`MAKE_REG`、`MAKE_SYM`、
  `TRUNCATE`

**防御什么：**
- 读取工作区之外的文件（例如 `/etc/passwd`、`~/.ssh`）
- 写入系统目录（`/usr`、`/bin`、`/lib`）
- 在受保护位置创建或删除文件

**不防御什么：**
- 网络访问（Landlock 只管文件系统）
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

**何时运行：** 只要 `/usr/bin/bwrap` 存在就会运行——bwrap 无条件赢得
Linux 后端选择，因为 Landlock 路径当前不做任何强制（`prepare_landlock`
是有文档记录的透传），真正的 bwrap 约束优于退化为无沙箱执行。旧版
顶层 `prefer_bwrap` 键与 `[sandbox] prefer_bwrap` 因此已废弃并被忽略。
作为
子命令的外层包装运行。

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

**回退：** 若未安装 bwrap，Landlock 后端会被报告为不可用——命令将
不加沙箱运行，因为 Landlock 当前不做任何强制。设置
`[sandbox] fail_if_unavailable = true`（或
`CODESMITH_SANDBOX_FAIL_IF_UNAVAILABLE=true`）可在请求的沙箱后端
不可用时选择失败关闭（fail closed），而不是不加沙箱继续运行。

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
permitted`、`sandbox-exec`、`deny(` 和 `Sandbox: `。（`file-write` 和
`network` 只出现在人类可读的拒绝消息中，不用于检测。）

### 6. Windows——未实现

CodeSmith 不提供进程内 Windows 沙箱。`windows.rs` 是一个存根：
`is_available()` 返回 false，`prepare_windows` 直接放行为无沙箱执行。
不存在任何 Job Object、内存上限或受限令牌代码。未来的 Windows 支持
必须通过专用辅助进程提供带 Job Object 和
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的进程树遏制，并且在文件系统、
网络、注册表或 AppContainer 隔离真正实现并单独测试之前，不得宣称
这些保证。

## 纵深防御

在 Linux 上，bwrap 的命名空间隔离是当前唯一的强制执行层，且仅在
bwrap 已安装时生效。prctl/rlimit 进程加固模块和 Landlock 规则集代码
存在于代码库中但未接入强制执行路径，seccomp 过滤器已移除：

```
bwrap (namespace isolation)  ← Linux 唯一的强制执行层（已安装时）
```

Linux 上未安装 bwrap 时，本地没有任何强制执行后端：`decide_sandbox`
报告不可用，命令将不加沙箱运行，除非 `[sandbox] fail_if_unavailable
= true` 使其失败关闭。在 macOS 上，强制执行层是 Seatbelt。

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
fail_if_unavailable = false       # true = fail closed instead of unsandboxed fallback
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
| macOS Seatbelt | sandbox-exec 违规 | 非零 | `Operation not permitted`、`sandbox-exec`、`deny(`、`Sandbox: ` |
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

- **Linux：** bubblewrap 在已安装时提供命名空间视图；未安装时本地
  没有任何强制执行后端，除非配置失败关闭，否则命令不加沙箱运行。
  主机允许列表网络策略不在本地强制执行。
- **macOS：** Seatbelt 配置文件在运行时生成。配置不当的配置文件
  可能过于宽松。
- **Windows：** 未实现任何沙箱；命令不加沙箱运行。

## 相关

- `crates/cli/src/sandbox/` — 宿主侧策略准备、运行时接线、外部后端（`opensandbox.rs`）
- `crates/agent-runtime/src/sandbox/` — 强制执行辅助程序（`seatbelt.rs`、`landlock.rs`、`windows.rs`、`bwrap.rs`、`process_hardening.rs`；由 `crates/cli/src/sandbox/` 垫片重导出）
- `crates/config/src/lib.rs` — 配置键
- `crates/tool-impls/src/tools/diagnostics.rs` — `diagnostics` 工具报告
  `sandbox_available`、`sandbox_type`、`bwrap_available`、
  `cgroup_version`
- `config.example.toml` — 带注释的配置参考
