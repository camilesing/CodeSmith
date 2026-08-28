# 沙箱威胁模型

CodeSmith 执行由 AI 推理派生的 shell 命令。沙箱模块限制这些命令对宿主
系统能做的事。本文档描述每个平台的沙箱实际强制执行什么、哪些是尽力
而为，以及哪些明确超出范围。

## 平台概览

| 机制 | 平台 | 类型 | 状态 |
|---|---|---|---|
| Seatbelt | macOS | 强制访问控制 | 已强制执行 |
| Landlock | Linux | 文件系统访问控制 | 已强制执行 |
| seccomp BPF | Linux | 系统调用过滤 | 已强制执行 |
| 进程加固 | Linux | 内核 prctl / rlimit | 已强制执行 |
| Bubblewrap (bwrap) | Linux | 命名空间隔离 | 可选 |
| Windows Job Object | Windows | 进程树遏制 | v1（PR #2220） |

## 威胁模型：每一层针对什么

### 1. 进程加固（仅 Linux）

**何时运行：** 在派生任何线程之前、Tokio 启动之前、任何数据载入内存
之前。

**做什么：**

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

**何时运行：** 在派生时通过辅助脚本或 `landlock_restrict_self` 应用
于每个子进程。只能由进程自身施加限制——父进程无法强制子进程进入
Landlock。

**做什么：**
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
- 进程检查（这件事用 seccomp）
- 读取已被映射的文件（Landlock 在 `open()` 时生效）

**检测：** `detect_denial()` 检查 stderr 中的 `Permission denied`、
`Operation not permitted`、`EACCES`、`EPERM`。

### 3. seccomp BPF（仅 Linux）

**何时运行：** 通过 `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)`
安装在子进程上。

**做什么：**
- 约 100 个安全系统调用的白名单（文件 I/O、内存、进程、IPC、同步、
  信号、时间）
- **明确拒绝：** `ptrace`、`mount`、`umount2`、`kexec_load`、
  `kexec_file_load`、`init_module`、`finit_module`、`delete_module`、
  `bpf`、`reboot`、`swapon`、`swapoff`、`pivot_root`、
  `setuid`/`setgid`/`setreuid`/`setregid`/`setresuid`/`setresgid`、
  `personality`
- 任何不在白名单上的系统调用 → `SECCOMP_RET_KILL_PROCESS`（SIGSYS）

**防御什么：**
- 通过 ptrace 劫持进程
- 挂载文件系统（绕过 Landlock 只读限制）
- 加载内核模块
- 加载 BPF 程序（否则会绕过 seccomp 本身！）
- 重启系统
- 通过 setuid/setgid 的特权变更

**不防御什么：**
- 将允许的系统调用用于恶意目的的合法使用
- 通过允许的系统调用进行侧信道攻击（例如时序）

**检测：** `detect_denial()` 检查退出码 31（SIGSYS）或 stderr 中的
`Bad system call`、`bad system call`、`SIGSYS`、`seccomp`。

### 4. Bubblewrap / bwrap（Linux，可选）

**何时运行：** 当 `/usr/bin/bwrap` 存在**并且**设置了旧版
`prefer_bwrap = true` 键或 `[sandbox] prefer_bwrap = true` 时。作为
子命令的外层包装运行。

**做什么：**
- 用 `--unshare-all` 创建新的挂载命名空间
- 将整个根文件系统以只读方式绑定挂载
- 以读写方式绑定挂载工作区目录
- 用 `--chdir` 切换到工作区

**防御什么：**
- 工作区之外的任何文件系统写入（比单独的 Landlock 更强，因为它在
  命名空间级别强制执行，而不仅是文件系统访问）
- 意外修改系统文件

**不防御什么：**
- 网络访问（bwrap 在 `--unshare-all` 下默认不创建网络命名空间；
  子进程仍有完整的网络访问）
- 进程检查
- 内存攻击

**安装：** 用户必须自行安装 bubblewrap：
- Ubuntu/Debian: `apt install bubblewrap`
- Fedora: `dnf install bubblewrap`
- Arch: `pacman -S bubblewrap`

CodeSmith 不会内置（vendor）bwrap。

**回退：** 若未安装 bwrap，CodeSmith 回退到宽松模式下的 Landlock。
设置 `[sandbox] fail_if_unavailable = true`（或
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

**检测：** 检查 stderr 中的 `file-write` 和 `network` 拒绝模式。

### 6. Windows Job Object（v1，PR #2220）

**何时运行：** 在进程派生时通过 `PROC_THREAD_ATTRIBUTE_JOB_LIST`
和受限令牌（restricted token）分配应用。

**做什么（v1）：**
- 带有 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的 Job Object——父进程
  退出时所有子进程终止
- 内存上限：每进程 1 GB，每作业 2 GB
- 活跃进程上限：64
- UI 限制：禁止访问桌面句柄
- 受限令牌：丢弃 Administrators 组 SID，设置为 medium-low 完整性
  级别

**推迟到 v2 的内容：**
- WFP（Windows Filtering Platform）防火墙规则——v1 中网络是开放的
- 派生时的文件系统 ACL 集成（存根已就位）
- AppContainer 隔离
- 注册表键隔离

**检测：** 检查 stderr 中的 `Access is denied`、`STATUS_ACCESS_DENIED`、
`ERROR_ACCESS_DENIED`、`ERROR_PRIVILEGE_NOT_HELD`、
`ERROR_ACCESS_DISABLED_BY_POLICY`，以及完整性/AppContainer 模式。

## 纵深防御

Linux 沙箱按顺序应用各层：

```
Process hardening (prctl)    ← before threads
    ↓
Landlock (filesystem)        ← at child spawn
    ↓
seccomp BPF (syscalls)       ← at child spawn
    ↓
bwrap (namespace isolation)  ← optional outer wrapper
```

每一层针对不同的威胁面。seccomp 无法保护文件系统（那是 Landlock 的
职责）。Landlock 无法阻止 ptrace（那是 seccomp + PR_SET_DUMPABLE 的
职责）。bwrap 增加了 Landlock 和 seccomp 都无法提供的命名空间级隔离。

## 配置

`~/.codesmith/config.toml` 中的相关配置键：

```toml
# Sandbox policy mode
sandbox_mode = "workspace-write"  # read-only | workspace-write | danger-full-access | external-sandbox

# Linux bubblewrap passthrough
prefer_bwrap = false              # requires `bubblewrap` package installed

# Structured runtime policy. These keys are optional and layer on top of
# the legacy flat keys above.
[sandbox]
enabled = true
fail_if_unavailable = false       # true = fail closed instead of unsandboxed fallback
enabled_platforms = ["macos", "linux"]
excluded_commands = []            # program names or command prefixes
auto_allow_bash_if_sandboxed = true
prefer_bwrap = false

[sandbox.filesystem]
mode = "workspace-write"          # read-only | workspace-write | danger-full-access | external-sandbox
writable_roots = []
allow_read = []
deny_read = []
allow_write = []
deny_write = []
exclude_tmpdir = true
exclude_slash_tmp = false

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
- `CODESMITH_PREFER_BWRAP=true` → `prefer_bwrap`
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
| macOS Seatbelt | sandbox-exec 违规 | 非零 | `file-write`、`network` |
| Linux Landlock | EACCES / EPERM | 非零 | `Permission denied`、`Operation not permitted` |
| Linux seccomp | SIGSYS (31) | 31 或 159 | `Bad system call`、`SIGSYS` |
| Linux bwrap | 挂载/命名空间失败 | 非零 | 不定 |
| Windows | 拒绝访问 / 特权 | 非零 | `Access is denied`、`ERROR_PRIVILEGE_NOT_HELD` |

`SandboxManager` 上的 `was_denied()` 方法聚合所有平台特定的检查。
`denial_message()` 方法返回人类可读的解释。

## 局限

### 沙箱不防御什么

- **网络攻击** — 只有 macOS Seatbelt 能在本地阻断网络。除非加入
  实现网络策略的后端，Linux 和 Windows v1 都保持网络开放。因此
  `[sandbox.network].allow_managed_domains_only = true` 在本地 OS
  沙箱下按拒绝网络处理，并在外部沙箱请求中透传，交由后端强制执行。
- **Git hook/fsmonitor 执行** — 当没有提供显式索引的 `GIT_CONFIG_*`
  环境时，CodeSmith 会向 shell git 配置注入空的 `core.fsmonitor` 和
  `core.hooksPath` 值，防止工作区 git 配置在沙箱化工具调用期间启动
  宿主侧辅助程序。
- **内存攻击** — 没有任何平台能阻止子进程读取自己的内存或利用内存
  破坏漏洞
- **时序侧信道** — Linux 上允许的系统调用可用于基于时序的信息泄露
- **资源耗尽** — Linux 的作业对象限制内存和进程数，但不限制 CPU、
  文件描述符或磁盘 I/O
- **内核漏洞** — 若内核本身存在漏洞，沙箱无法阻止利用（这适用于
  所有平台）
- **供应链** — 若子进程下载并执行不可信代码，沙箱限制该代码能做的
  事，但不阻止下载

### 平台特定缺口

- **Linux：** Landlock 保护文件系统访问，bubblewrap 在可用/被偏好时
  可提供命名空间视图。seccomp 增加系统调用过滤，但使用白名单，新
  系统调用可能需要更新。主机允许列表网络策略不在本地强制执行。
- **macOS：** Seatbelt 配置文件在运行时生成。配置不当的配置文件
  可能过于宽松。
- **Windows v1：** 派生时没有文件系统 ACL 强制执行。网络完全开放。
  Job Object 仅覆盖进程树。

## 相关

- `crates/tui/src/sandbox/` — 宿主侧策略准备、运行时接线、外部后端（`opensandbox.rs`）
- `crates/agent-runtime/src/sandbox/` — 强制执行辅助程序（`seatbelt.rs`、`landlock.rs`、`seccomp.rs`、`windows.rs`、`bwrap.rs`、`process_hardening.rs`；经 TUI 的 sandbox 模块重导出）
- `crates/config/src/lib.rs` — 配置键
- `crates/tool-impls/src/tools/diagnostics.rs` — `diagnostics` 工具报告
  `sandbox_available`、`sandbox_type`、`bwrap_available`、
  `cgroup_version`
- `config.example.toml` — 带注释的配置参考
- Issue #2180 — 本文档
- Issue #2182 — seccomp 过滤器实现
- Issue #2183 — 进程加固
- Issue #2184 — bwrap 透传
- Issue #2185 — Windows Job Object v1
- Issue #2186 — SandboxExecutor trait 统一
- Issue #2187 — 沙箱一致性测试
