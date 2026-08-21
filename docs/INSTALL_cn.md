# 安装 CodeSmith

本页面涵盖所有受支持的安装路径以及最常见的"安装失败"问题，包括 **Linux ARM64** 和其他较不常见的平台。

如果你只想要简短版本，请参阅[主 README](../README.md#quickstart) 或[简体中文 README](../README.zh-CN.md#快速开始)。

---

## 1. 支持的平台

CodeSmith 为以下平台/架构组合提供配套的 `codesmith` 和 `codesmith-tui` 预构建二进制。Linux ARM64 从 v0.8.8 起可用；Linux RISC-V 从 v0.8.47 之后的第一个版本开始提供。

| 平台     | 架构 | npm install | `cargo install` | GitHub release 资产                                  |
| ------------ | ------------ | :---------: | :-------------: | ----------------------------------------------------- |
| Linux        | x64 (x86_64) |     ✅      |       ✅        | `codesmith-linux-x64`, `codesmith-tui-linux-x64`        |
| Linux        | arm64        |     ✅      |       ✅        | `codesmith-linux-arm64`, `codesmith-tui-linux-arm64`    |
| Linux        | riscv64      |     ✅      |       ✅        | `codesmith-linux-riscv64`, `codesmith-tui-linux-riscv64`|
| macOS        | x64          |     ✅      |       ✅        | `codesmith-macos-x64`, `codesmith-tui-macos-x64`        |
| macOS        | arm64 (M-series) | ✅      |       ✅        | `codesmith-macos-arm64`, `codesmith-tui-macos-arm64`    |
| Windows      | x64          |     ✅      |       ✅        | `codesmith-windows-x64.exe`, `codesmith-tui-windows-x64.exe` |
| 其他 Linux（musl、其他架构） | — |   ❌¹    |       ✅²       | 从源码构建                                     |
| FreeBSD / OpenBSD              | — |   ❌      |       ✅²       | 从源码构建                                     |

¹ npm 包会以明确的错误退出并指引你到这里。
² 前提是你的工具链能够编译较新的 Rust workspace；见下文的[从源码构建](#7-build-from-source)。

Linux release 资产是 glibc 构建，而不是 musl 构建。它们动态链接常规的 Linux 运行时库，例如 `libdbus-1` 和 `libc`；SQLite 目前通过 `rusqlite` 打包进二进制，因此对于官方 release 资产，用户不需要单独安装 `libsqlite3` 运行时包。基于 musl 的系统（如 Alpine）应使用[从源码构建](#7-build-from-source)。

> **Linux ARM64 说明（v0.8.7 及更早版本）。** v0.8.7 及更早版本**不**发布 Linux ARM64 预构建；使用 HarmonyOS 轻薄本、Asahi Linux、树莓派、AWS Graviton 等的用户在 `npm i -g codesmith` 时会看到 `Unsupported architecture: arm64`。v0.8.8 发布了 `codesmith-linux-arm64` 和 `codesmith-tui-linux-arm64`，因此在任何基于 glibc 的 ARM64 Linux 上直接 `npm i -g codesmith` 即可。如果你停留在 v0.8.7，请跳到[从源码构建](#7-build-from-source) —— `cargo install` 可以正常工作。

---

## 2. 下载安全与校验和

官方 release 二进制仅从 `https://github.com/Hmbown/CodeSmith/releases` 和名为 `codesmith` 的 npm 包发布。除非你有意信任某个镜像，否则不要从相似的仓库、归档或搜索结果镜像安装 release 资产。

每个 GitHub release 都包含 `codesmith-artifacts-sha256.txt`。如果你手动下载二进制，请在运行前校验：

```bash
# Run from the directory containing the downloaded binaries.
curl -L -O https://github.com/Hmbown/CodeSmith/releases/latest/download/codesmith-artifacts-sha256.txt
sha256sum -c codesmith-artifacts-sha256.txt --ignore-missing
```

在 macOS 上，使用 `shasum -a 256 -c codesmith-artifacts-sha256.txt` 代替 `sha256sum`。

如果杀毒软件标记了官方 release 二进制，在确认具体工件之前请将其视为未解决。请在 GitHub issue 中包含以下所有内容：

- release tag，例如 `v0.8.36`
- 确切的下载 URL
- 文件名，例如 `codesmith-linux-x64`
- 你机器上文件的 SHA-256
- 杀毒软件产品名称和检出名称

这能让维护者区分官方工件的误报与来自仿冒仓库或镜像的下载。

---

## 3. 通过 npm 安装（推荐）

```bash
npm install -g codesmith
codesmith
```

`postinstall` 会从对应的 GitHub release 下载正确的二进制对，校验 SHA-256 清单，并将 `codesmith` 和 `codesmith-tui` 都暴露到你的 `PATH`。

有用的环境变量：

| 变量                            | 用途                                                                                |
| ----------------------------------- | -------------------------------------------------------------------------------------- |
| `DEEPSEEK_TUI_VERSION`              | 固定包装器下载的 release 版本（默认为 `deepseekBinaryVersion`）          |
| `DEEPSEEK_TUI_GITHUB_REPO`          | 让下载器指向某个 fork（`owner/repo`）                                          |
| `DEEPSEEK_TUI_RELEASE_BASE_URL`     | 覆盖下载根地址（例如内部镜像或 release 资产代理）            |
| `DEEPSEEK_TUI_FORCE_DOWNLOAD=1`     | 即使缓存的二进制标记匹配也重新下载                                     |
| `DEEPSEEK_TUI_DISABLE_INSTALL=1`    | 完全跳过 `postinstall` 下载（CI 冒烟、vendored 二进制）                 |
| `DEEPSEEK_TUI_OPTIONAL_INSTALL=1`   | 下载/解压出错时不让 `npm install` 失败 —— 在 CI 矩阵中很有用            |

> **中国大陆 npm 下载慢？** 如果 `npm install` 本身就慢（而不只是 postinstall 二进制下载慢），请使用 npm registry 镜像：
> ```bash
> npm config set registry https://registry.npmmirror.com
> npm install -g codesmith
> ```
> 如果你比起 npm 更偏好 Cargo，另见[第 4 节](#4-install-via-cargo-any-tier-1-rust-target)。

---

## 4. 通过 Cargo 安装（任意 Tier-1 Rust 目标）

如果 GitHub releases 慢、被屏蔽，或你使用的是不受支持的架构，请直接从 crates.io 安装。两个 crate 都是必需的 —— 调度器在运行时会委托给 TUI 运行时。

```bash
# Requires Rust 1.88+ (https://rustup.rs)
cargo install codesmith-cli --locked   # provides `codesmith`
cargo install codesmith-tui     --locked   # provides `codesmith-tui`
codesmith --version
```

### 中国 / 镜像友好的安装

在中国大陆安装时，请为 **rustup**（Rust 工具链安装器）和 **Cargo**（包 registry）都配置镜像，以避免 TLS 超时和下载失败。

**第 1 步：通过 rustup 镜像安装 Rust**

```bash
# PowerShell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
(New-Object Net.WebClient).DownloadFile('https://win.rustup.rs/x86_64', 'rustup-init.exe')

# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable

# Linux / macOS
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

如果你的网络访问 TUNA 镜像较慢，`rsproxy.cn` 是 Linux/macOS 上另一个 rustup 镜像选择：

```bash
export RUSTUP_DIST_SERVER=https://rsproxy.cn
export RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

`RUSTUP_DIST_SERVER` 和 `RUSTUP_UPDATE_ROOT` 环境变量必须在运行 rustup-init **之前**设置；否则工具链下载会遇到与安装器相同的 TLS 握手问题。

**第 2 步：配置 Cargo registry 镜像**

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

`rsproxy`、腾讯 COS 和阿里云 OSS 镜像的工作方式相同；选择在你的网络上最快的一个。

### 腾讯云远程优先设置

对于可以通过手机控制的常驻工作区，请使用腾讯原生路径，而不是把安装当作笔记本电脑上的一次性步骤：

- CNB 镜像/源：`https://cnb.cool/codesmith.net/codesmith.git`
- 腾讯云轻量应用服务器（香港）：`/opt/whalebro` 远程工作区
- 飞书/Lark：长连接手机桥接
- EdgeOne：可选的公共 HTTPS 边缘，用于文档/状态/webhook 面

请从[腾讯云远程优先快速开始](TENCENT_CLOUD_REMOTE_FIRST.md)入手，然后按照[腾讯云轻量（香港）手机设置](TENCENT_LIGHTHOUSE_HK.md)操作。

---

## 5. 通过 Nix 安装

**试一试**

如果你已经有支持 flake 的 Nix，请运行：

```sh
nix run github:Hmbown/CodeSmith
```

Nix 会构建 `codesmith-tui`，然后启动 `codesmith` 调度器。在 `--` 之后传递参数，例如：

```sh
nix run github:Hmbown/CodeSmith -- --help
```

### Flake

向 `flake.nix` 添加输入：

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    codesmith-tui.url = "github:Hmbown/CodeSmith";
    codesmith-tui.inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

安装到 NixOS 模块：

```nix
{
  outputs = { self, nixpkgs, codesmith-tui }:
  let
    # replace system "x86_64-linux" with your system
    system = "x86_64-linux";
  in
  {
    # change `yourhostname` to your actual hostname
    nixosConfigurations.yourhostname = nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        # ...
        {
          environment.systemPackages = [ codesmith-tui.packages.${system}.default ];
        }
      ];
    };
  };
}
```

---

## 6. 从 GitHub Releases 手动下载

从 [Releases 页面](https://github.com/Hmbown/CodeSmith/releases)获取与你的平台匹配的二进制对，并将它们并排放到你 `PATH` 上的某个目录（例如 `~/.local/bin`）：

```bash
# Linux ARM64 example
mkdir -p ~/.local/bin
curl -L -o ~/.local/bin/codesmith      \
    https://github.com/Hmbown/CodeSmith/releases/latest/download/codesmith-linux-arm64
curl -L -o ~/.local/bin/codesmith-tui  \
    https://github.com/Hmbown/CodeSmith/releases/latest/download/codesmith-tui-linux-arm64
chmod +x ~/.local/bin/codesmith ~/.local/bin/codesmith-tui
codesmith --version
```

> **macOS Gatekeeper 说明。** 如果你用浏览器下载了这些二进制，macOS 可能会以"Apple 无法验证"警告拦截它们。清除两个二进制上的隔离属性后重试：
> ```bash
> xattr -d com.apple.quarantine ~/.local/bin/codesmith ~/.local/bin/codesmith-tui 2>/dev/null || true
> ```

根据每个 release 的 SHA-256 清单校验完整性：

```bash
curl -L -o /tmp/codesmith-artifacts-sha256.txt \
    https://github.com/Hmbown/CodeSmith/releases/latest/download/codesmith-artifacts-sha256.txt
( cd ~/.local/bin && sha256sum -c /tmp/codesmith-artifacts-sha256.txt --ignore-missing )
```

（在 macOS 上使用 `shasum -a 256 -c` 代替 `sha256sum`。）

### Windows Scoop

`codesmith` 包已列入 Scoop 的 main bucket：

```powershell
scoop update
scoop install codesmith
codesmith --version
```

Scoop 清单在本仓库的发布流程之外维护，可能落后于 GitHub/npm/Cargo 发布。当你需要立即获得最新版本时，请使用 npm 或手动下载 GitHub release。

---

## 7. 从源码构建

这是我们未提供预构建的所有平台的兜底方案 —— 包括 musl、riscv64、LoongArch、FreeBSD 和 2024 年以前的 ARM64 发行版。

### 前置条件

- **Rust** 1.88 或更高版本 —— 使用 [rustup](https://rustup.rs) 安装。
- **Linux 构建期依赖**（Debian/Ubuntu/openEuler/Kylin）：
  ```bash
  sudo apt-get install -y build-essential pkg-config libdbus-1-dev
  # openEuler / RHEL family:
  # sudo dnf install -y gcc make pkgconf-pkg-config dbus-devel
  ```
- 不**需要**可用的 `cmake`。

### 构建并安装

```bash
git clone https://github.com/Hmbown/CodeSmith.git
cd CodeSmith

cargo install --path crates/cli --locked   # provides `codesmith`
cargo install --path crates/tui --locked   # provides `codesmith-tui`

codesmith --version
```

默认情况下两个二进制都会落在 `~/.cargo/bin/`；请确保该目录在你的 `PATH` 上。

### 从 x64 交叉编译到 ARM64 Linux

如果你想在 x64 Linux 主机上构建 ARM64 Linux 二进制（例如用于 HarmonyOS / openEuler ARM64 轻薄本），请使用 [`cross`](https://github.com/cross-rs/cross)，它把官方 Rust 交叉目标封装在 Docker 容器中：

```bash
# Once
rustup target add aarch64-unknown-linux-gnu
cargo install cross --locked

# Per build
cross build --release --target aarch64-unknown-linux-gnu -p codesmith-cli
cross build --release --target aarch64-unknown-linux-gnu -p codesmith-tui
```

生成的二进制位于 `target/aarch64-unknown-linux-gnu/release/codesmith` 和 `target/aarch64-unknown-linux-gnu/release/codesmith-tui`。将匹配的一对复制到 ARM64 主机（例如通过 `scp`）并对它们执行 `chmod +x`。

如果你没有 Docker，可以直接安装交叉链接器，让 Cargo 完成其余工作：

```bash
sudo apt-get install -y gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu

cat >> ~/.cargo/config.toml <<'EOF'
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
EOF

cargo build --release --target aarch64-unknown-linux-gnu -p codesmith-cli
cargo build --release --target aarch64-unknown-linux-gnu -p codesmith-tui
```

如果你的发行版基于 musl，同样的配方适用于 `aarch64-unknown-linux-musl`。

### Windows 从源码构建

在 Windows 上构建需要来自 [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022) 的 **MSVC C 工具链**（免费的可选工作负载安装器，而不是完整 IDE）。

**前置条件（Windows）**

1. 安装 Visual Studio 2022 Build Tools —— 选择 **"Desktop development with C++"** 工作负载。
2. 安装 [Rust](https://rustup.rs) 1.88+（如果从中国大陆下载，请参阅上文的[中国镜像安装说明](#china--mirror-friendly-install)）。
3. 安装 [Git for Windows](https://git-scm.com/download/win)（提供 `git` 和 `git-bash` 终端）。

**推荐终端**：Windows Terminal、`git-bash` 或 PowerShell。`cmd.exe` 也能用，但缓冲区小且 PATH 行为受限。

**配置 MSVC 环境**

Visual Studio Build Tools 会把 `cl.exe` 安装到带版本号的目录，但**不会**将其全局添加到 `PATH`。你必须手动设置环境或使用 Developer Command Prompt。所需的变量包括：

```powershell
# Adjust version numbers to match your installation
$msvc = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207"
$sdk   = "C:\Program Files (x86)\Windows Kits\10"
$sdkv  = "10.0.26100.0"

$env:INCLUDE  = "$msvc\include;$msvc\atlmfc\include;$sdk\Include\$sdkv\ucrt;$sdk\Include\$sdkv\um;$sdk\Include\$sdkv\shared"
$env:LIB      = "$msvc\lib\x64;$msvc\atlmfc\lib\x64;$sdk\Lib\$sdkv\ucrt\x64;$sdk\Lib\$sdkv\um\x64"
$env:LIBPATH  = "$msvc\lib\x64;$msvc\atlmfc\lib\x64"
$env:CC       = "$msvc\bin\Hostx64\x64\cl.exe"
$env:CXX      = "$msvc\bin\Hostx64\x64\cl.exe"
$env:PATH     = "$msvc\bin\Hostx64\x64;$env:PATH"
```

或者，打开 **"Developer Command Prompt for VS 2022"**（安装 Build Tools 后可从"开始"菜单找到），它会运行 `vcvars64.bat` 自动配置上述所有内容。然后在该会话中将 `cargo` 加入 `PATH`，并从项目根目录运行 `cargo build`。

**Cargo registry 镜像** —— 在 Windows 上，镜像配置位于 `%USERPROFILE%\.cargo\config.toml`。见[上文的第 2 步](#china--mirror-friendly-install)。

**构建**

```bash
git clone https://github.com/Hmbown/CodeSmith.git
cd CodeSmith
set CARGO_HTTP_CHECK_REVOKE=false   # may be needed behind some Chinese ISPs
cargo build --release
```

两个二进制分别出现在 `target\release\codesmith.exe` 和 `target\release\codesmith-tui.exe`。

> **在 Windows 上，除非你需要修改源码，否则优先使用 `npm install -g`。** npm 包会拉取预构建二进制，完全避免 C 工具链依赖 —— 见[第 3 节](#3-install-via-npm-recommended)。

---

## 8. 故障排查

### `Unsupported architecture: arm64 on platform linux`

你使用的是早于 v0.8.8、不发布 Linux ARM64 二进制的版本。要么升级（`npm i -g codesmith@latest`），要么按照[第 4 节](#4-install-via-cargo-any-tier-1-rust-target)使用 `cargo install`。

### 运行时出现 `MISSING_COMPANION_BINARY`

调度器（`codesmith`）要求 TUI 运行时（`codesmith-tui`）位于同一 `PATH` 上。如果你通过 `cargo install` 只安装了一个 crate，请两个都安装：

```bash
cargo install codesmith-cli --locked
cargo install codesmith-tui     --locked
```

### `codesmith update` 报告 `no asset found for platform codesmith-linux-aarch64`

这是 v0.8.7 中的 [#503](https://github.com/Hmbown/CodeSmith/issues/503) —— 自更新器使用了 Rust 的 `aarch64`/`x86_64` 架构名，而不是 release 工件的 `arm64`/`x64`。在 v0.8.8 之前的解决办法：

```bash
npm i -g codesmith@latest
# or
cargo install codesmith-cli --locked
```

### 中国大陆 npm 下载慢或超时

将 `DEEPSEEK_TUI_RELEASE_BASE_URL` 设置为镜像的 release 资产目录（rsproxy、TUNA、腾讯 COS、阿里云 OSS），或者完全跳过 npm，使用[第 4 节](#4-install-via-cargo-any-tier-1-rust-target)中的 Cargo 镜像配置。

### 中国大陆 `codesmith update` 被 GitHub 屏蔽

`codesmith update` 通常会访问 GitHub Releases 获取元数据和二进制资产。在 GitHub 被屏蔽或不稳定的网络上，请改用 CNB 源镜像，并从 release tag 安装两个二进制：

要在不下载或替换二进制的情况下检查最新版本，请运行 `codesmith update --check`。

```bash
cargo install --git https://cnb.cool/codesmith.net/codesmith --tag vX.Y.Z codesmith-cli --locked --force
cargo install --git https://cnb.cool/codesmith.net/codesmith --tag vX.Y.Z codesmith-tui     --locked --force
```

如果你运营二进制资产镜像，`codesmith update` 可以直接使用它：

```bash
DEEPSEEK_TUI_VERSION=X.Y.Z \
DEEPSEEK_TUI_RELEASE_BASE_URL=https://your-mirror.example.com/DeepSeek-TUI/vX.Y.Z/ \
codesmith update
```

镜像目录必须包含 `codesmith-artifacts-sha256.txt` 以及 GitHub release 中的各平台二进制。

### Debian/Ubuntu：`cargo install` 报 `feature edition2024 is required`

一些 Debian/Ubuntu 发行版自带的 Cargo 较旧，无法解析 Rust 2024 crate。例如，Ubuntu 24.04 上的 Cargo 1.75.0 会在构建前失败并显示：

```text
feature `edition2024` is required
The package requires the Cargo feature called `edition2024`, but that feature
is not stabilized in this version of Cargo
```

请通过 rustup 安装当前的稳定版 Rust，然后重新运行[第 4 节](#4-install-via-cargo-any-tier-1-rust-target)中的两条 Cargo install 命令。对于中国大陆网络，以下基于 rsproxy 的流程已验证可用：

```bash
export RUSTUP_DIST_SERVER=https://rsproxy.cn
export RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup default stable
cargo install codesmith-cli --locked
cargo install codesmith-tui     --locked
```

之后，`which cargo` 应指向 `~/.cargo/bin/cargo`，而不是 `/usr/bin/cargo`。

### Debian/Ubuntu：构建时报 `error: linker 'cc' not found`

安装 C 工具链：

```bash
sudo apt-get install -y build-essential pkg-config libdbus-1-dev
```

### 包装器已安装但找不到 `codesmith`

`npm i -g` 会安装到 `$(npm prefix -g)/bin`；请确保该目录在你 shell 的 `PATH` 上。使用 nvm 时：`nvm use --lts && hash -r`。

### Windows：`rustup-init` 报 `TLS handshake eof` 或 `CRYPT_E_REVOCATION_OFFLINE`

在 GFW 或某些中国 ISP 之后的网络到 `static.rust-lang.org` 的 TLS 握手会失败。请在运行安装器**之前**设置 rustup 镜像环境变量：

```bash
# git-bash / msys2
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
export RUSTUP_UPDATE_ROOT=https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup
./rustup-init.exe -y --default-toolchain stable
```

如果安装 Rust 后 Cargo 报 `CRYPT_E_REVOCATION_OFFLINE`，还需在 `cargo build` 期间设置 `CARGO_HTTP_CHECK_REVOKE=false`。

### Windows：`cargo build` 期间找不到 MSVC 编译器（`cl.exe`）

Visual Studio Build Tools 不会将 `cl.exe` 添加到全局 `PATH`。可以：

1. 从"开始"菜单打开 **"Developer Command Prompt for VS 2022"**，在该窗口中将 `%USERPROFILE%\.cargo\bin` 加入 `PATH`，然后从那里运行 `cargo build`；或
2. 手动设置 MSVC 环境变量 —— PowerShell 片段见 [Windows 从源码构建](#windows-build-from-source)一节。

验证编译器可访问：`cl.exe /?` 应打印帮助文本。

### Windows：Cargo 执行构建脚本时报 `拒绝访问 (os error 5)`

第三方杀毒软件（火绒、360、卡巴斯基等）可能阻止 Cargo 执行刚编译出的构建脚本二进制（例如 `libsqlite3-sys`、`aws-lc-sys`、`instability`）。该错误与路径无关 —— 移动 `target-dir` 没有帮助。

**症状**：`could not execute process ... build-script-build (never executed)`

**解决办法**（任选其一）：

1. **将项目的 `target/` 目录加入杀毒软件排除列表。**
2. 在 `cargo build` 期间**临时关闭杀毒软件**。
3. **改用 `npm install -g codesmith`** —— npm 包附带预构建二进制，完全跳过 Cargo 构建（[第 3 节](#3-install-via-npm-recommended)）。
4. 从 crates.io **使用 `cargo install codesmith-cli --locked`** —— 这会改变二进制路径，某些杀毒工具对此的处理不同。

要验证构建脚本二进制本身有效（未损坏），请在 `target/debug/build/<crate>/build-script-build` 下找到它并手动运行：

```bash
target/debug/build/libsqlite3-sys-*/build-script-build
# If this runs but panics with "NotPresent" (no C compiler), the binary is
# fine — the AV is blocking Cargo's process-spawning path specifically.
```

### npm 二进制下载超时

如果 `codesmith` 在从 `github.com` 拉取时等待数秒并打印 `connect ETIMEDOUT` 或 `EAI_AGAIN`，说明 npm 包装器已成功安装，但从 GitHub Releases 下载预构建二进制在你的网络上被屏蔽或不稳定。该下载与 npm registry 包下载是分开的。

使用以下途径之一：

1. 设置代理并重试：

   ```bash
   export HTTPS_PROXY=http://your-proxy:port
   codesmith
   ```

2. 在内部镜像 release 资产并设置 `DEEPSEEK_TUI_RELEASE_BASE_URL`：

   ```bash
   export DEEPSEEK_TUI_RELEASE_BASE_URL=https://your-mirror.example.com/DeepSeek-TUI/
   codesmith
   ```

   该目录必须包含 `codesmith-artifacts-sha256.txt` 以及 GitHub release 中的各平台二进制。

3. 通过 Cargo 安装，它在本地构建，不会下载 GitHub release 资产。见[第 4 节](#4-install-via-cargo-any-tier-1-rust-target)。

4. 从 [Releases 页面](https://github.com/Hmbown/CodeSmith/releases)手动下载 `codesmith` 和 `codesmith-tui`，放到 `PATH` 上的目录中并赋予可执行权限。见[第 6 节](#6-manual-download-from-github-releases)。

---

## 9. 验证安装

```bash
codesmith --version
codesmith doctor       # checks API key, provider, runtime, and PATH integrity
codesmith doctor --json
```

`doctor` 在发现问题时以非零值退出，并打印结构化的修复提示。如果需要帮助，请将 JSON 输出粘贴到 GitHub issue 中。
