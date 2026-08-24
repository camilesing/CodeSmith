# CodeSmith 发布运行手册

本手册是发布 Rust crate、GitHub release 资产以及 `codesmith`
npm 包装器的权威依据。

当前打包说明：
- `codesmith-tui` 是当前交付给用户的实际运行时 crate。
- `codesmith-tui-core` 是为拆分/对齐工作服务的工作区支撑 crate，并不是交付运行时的替代品。

## 规范发布目标

- 面向最终用户的 crate：
  - `codesmith-tui`
  - `codesmith-cli`
- 从本工作区发布的支撑 crate（顺序与 `scripts/release/crates.sh` 一致）：
  - `codesmith-secrets`
  - `codesmith-config`
  - `codesmith-protocol`
  - `codesmith-state`
  - `codesmith-agent`
  - `codesmith-execpolicy`
  - `codesmith-hooks`
  - `codesmith-mcp`
  - `codesmith-tools`
  - `codesmith-core`
  - `codesmith-app-server`
  - `codesmith-tui-core`
- 其余工作区 crate（`codesmith-agent-runtime`、`codesmith-providers`、
  `codesmith-tool-impls`、`codesmith-index`、`codesmith-extensions`、
  `codesmith-release`）**不**发布——它们以路径依赖的形式被已发布的
  二进制消费。

## 版本协调

- Rust crate 继承 [Cargo.toml](../Cargo.toml) 中的共享工作区版本。
- 内部路径依赖的版本应与共享工作区版本一致；一旦工作区版本变动，陈旧的旧版本锁定会成为发布阻塞项。
- npm 包装器的版本位于 [npm/codesmith/package.json](../npm/codesmith/package.json)。
- `codesmithBinaryVersion` 决定 npm 包装器下载哪个 GitHub release 二进制文件。
- 允许仅打包的 npm 发布：
  - 提升 npm 包版本
  - 保持 `codesmithBinaryVersion` 固定在之前已发布的 Rust 二进制版本上
  - 在 `npm publish` 之前重新运行 `npm pack` 冒烟检查

## 预检（Preflight）

打 tag 之前，在仓库根目录运行以下命令：

```bash
./scripts/release/check-versions.sh   # version drift between workspace, npm, lockfile
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo publish --dry-run --locked --allow-dirty -p codesmith-tui
./scripts/release/publish-crates.sh dry-run
```

`check-versions.sh` 也会在 CI 中于每次 push/PR 时运行
（`.github/workflows/ci.yml` 中的 `versions` 作业），因此
`Cargo.toml`、各 crate 清单、`npm/codesmith/package.json` 和
`Cargo.lock` 之间的漂移会在发布之前而不是发布之时被发现。

受版本控制的 CNB 流水线为 `fix/*`、`rebrand/*`、`work/v*` 和
`main` 镜像了重量级的 Linux 版本/fmt/check/clippy/test/npm-smoke
门禁。GitHub Actions 保留轻量的漂移/fmt 状态以及 macOS 和
Windows 覆盖，而 Linux 的工作由 CNB 承担。

`publish-crates.sh dry-run` 会对没有未发布工作区依赖的 crate 执行
完整的 `cargo publish --dry-run`，并对依赖工作区的 crate 执行打包
预检。这样既避免了 crates.io 尚未包含新工作区版本时产生的误报，
又能在发布前验证包内容。

对于 npm 包装器的验证，需构建两个交付的二进制文件并运行跨平台
冒烟测试工具。它会打包 npm 包装器、将其安装到一个干净的临时
项目中、通过 HTTP 提供本地 release 资产，并检查分发器到 TUI 的
路径（`codesmith doctor --help`）和直接 TUI 入口
（`codesmith-tui --help`）。

```bash
cargo build --release --locked -p codesmith-cli -p codesmith-tui
node scripts/release/npm-wrapper-smoke.js
```

设置 `DEEPSEEK_TUI_KEEP_SMOKE_DIR=1` 可以保留临时的打包/安装
目录以供检查。

若还要在本地演练 `npm run release:check`，请在启动服务器之前，
使用完整的资产矩阵夹具重新生成本地资产目录：

```bash
DEEPSEEK_TUI_PREPARE_ALL_ASSETS=1 node scripts/release/prepare-local-release-assets.js
cd npm/codesmith
DEEPSEEK_TUI_VERSION=X.Y.Z DEEPSEEK_TUI_RELEASE_BASE_URL=http://127.0.0.1:8123/ npm run release:check
```

将该次本地运行中 `DEEPSEEK_TUI_VERSION` 设置为你要验证的 npm
包版本。

CNB 工作流运行 Linux tarball 安装 + 委托入口点冒烟测试；
GitHub Actions 保留 macOS 和 Windows 的冒烟覆盖。

发布之后，证明 release 在两个 registry 中均可见：

```bash
./scripts/release/check-published.sh X.Y.Z
```

在该命令看到 npm 上的 `codesmith@X.Y.Z` 以及 crates.io 上每个
`codesmith-*` crate 都处于 `X.Y.Z` 之前，不要将 Rust 发布标记为
完成。对于少见的仅 npm 打包发布，需使用
`--allow-npm-binary-mismatch` 运行，并在 release 说明中明确声明
没有交付新的 Rust 二进制版本。

## Rust Crates 发布

向 crates.io 发布 crate 是**手动的**——没有自动化的
`crates-publish` GitHub 工作流。操作者需在已配置 `cargo login`
的开发者工作站上运行 `scripts/release/` 中的辅助脚本。

1. 更新 [Cargo.toml](../Cargo.toml) 中的工作区版本。
2. 在本地运行 `./scripts/release/check-versions.sh` 和
   `./scripts/release/publish-crates.sh dry-run`；两者都必须干净通过。
3. 将发布打上 `vX.Y.Z` 的 tag（通常做法是把版本号提升推送到
   `main`，让 `auto-tag.yml` 创建 tag——关于 `RELEASE_TAG_PAT`
   的要求，见下文 npm 包装器发布一节）。
4. 按以下顺序使用 `./scripts/release/publish-crates.sh publish` 发布 crate：
   - `codesmith-secrets`
   - `codesmith-config`
   - `codesmith-protocol`
   - `codesmith-state`
   - `codesmith-agent`
   - `codesmith-execpolicy`
   - `codesmith-hooks`
   - `codesmith-mcp`
   - `codesmith-tools`
   - `codesmith-core`
   - `codesmith-app-server`
   - `codesmith-tui-core`
   - `codesmith-cli`
   - `codesmith-tui`
5. 等待每个已发布的 crate 版本出现在 crates.io 上，再发布其依赖者。

发布辅助脚本对重复运行是幂等的：已发布的 crate 版本会被跳过。

## GitHub Release 资产

`.github/workflows/release.yml` 构建以下二进制文件：

- `codesmith-linux-x64`
- `codesmith-macos-x64`
- `codesmith-macos-arm64`
- `codesmith-windows-x64.exe`
- `codesmith-tui-linux-x64`
- `codesmith-tui-macos-x64`
- `codesmith-tui-macos-arm64`
- `codesmith-tui-windows-x64.exe`

release 作业还会上传 `codesmith-artifacts-sha256.txt`。npm 安装器和
发布验证脚本都依赖该校验和清单。

## npm 包装器发布

**npm publish 步骤是手动的。** `release.yml` 不再运行
`npm publish`，因为 npm 账户在每次发布时都要求 2FA OTP，而且尚未
配置可绕过 2FA 的自动化 token。GitHub Release 流程仍然完全自动
化；只有 npm 包装器的发布需要开发者在装有 `npm login` 和身份验证
器应用的工作站上执行。

### 步骤

1. 将 [npm/codesmith/package.json](../npm/codesmith/package.json) 中的 npm 包版本设置为与工作区 `Cargo.toml` 一致。CI 的版本漂移守卫会在打 tag 之前发现不匹配。
2. 将 `codesmithBinaryVersion` 设置为应提供二进制文件的 GitHub release tag。
3. 将版本号提升推送到 `main`。`auto-tag.yml` 会创建对应的 `vX.Y.Z` tag，`release.yml` 会构建二进制矩阵并起草 GitHub Release。
4. **等待 GitHub Release 完成**，包含全部八个签名二进制文件以及 `codesmith-artifacts-sha256.txt`。npm 的 `prepublishOnly` 钩子（`scripts/verify-release-assets.js`）要求每个资产都必须存在。
5. 在开发者机器上手动发布 npm 包装器：

```bash
cd npm/codesmith
npm publish --access public
# (you will be prompted for the npm OTP from your authenticator)
```

### 为什么不自动化？

- `release.yml` 旧的 `publish-npm` 作业使用 `secrets.NPM_TOKEN`，但 npm 默认启用 2FA 的策略意味着发布 token 必须是启用了"Bypass 2FA for token authentication"的自动化 token，或者是账户级关闭 2FA 的状态。这两种我们都没有配置。
- 独立的 `publish-npm.yml` 和 `crates-publish.yml` 工作流已被移除；没有残留的闲置自动化管线。未来如果转向 npm Trusted Publishing（OIDC），届时会重新引入一个专用工作流。

### 如果之后修复了 token

要重新启用自动发布：配置一个启用了"Bypass 2FA for token authentication"的 npm 自动化 token（或通过 OIDC 设置 npm Trusted Publishing），将相应的 secret 存储到仓库中，并向 `release.yml`（或一个专用工作流）重新添加 `publish-npm` 作业，同时撤销本节的"手动"表述。

## CNB Cool 镜像

对 `main`、`fix/*`、`rebrand/*`、`work/v*` 的每次推送以及每个
`v*` tag 都会通过 `Sync to CNB` 工作流镜像到
`cnb.cool/codesmith.net/codesmith`，这样位于 GitHub 被屏蔽网络
中的用户可以获取源码，CNB 也可以运行重量级的 Linux CI 泳道。
发布 tag 之后，在宣布发布完成之前，**请验证镜像已捕获该 tag**：

```bash
git ls-remote https://cnb.cool/codesmith.net/codesmith.git refs/tags/vX.Y.Z
```

如果该工作流在发布 tag 上失败了，手动回退方案记录在
[docs/CNB_MIRROR.md](CNB_MIRROR.md) 中（一次性执行 `git
remote add cnb …`，然后 `git push cnb vX.Y.Z`）。

## 恢复与回滚

- Crate 部分发布：
  - 重新运行 `./scripts/release/publish-crates.sh publish`
  - 已发布的 crate 版本会被跳过
- GitHub 资产缺失或校验和清单不完整：
  - 修复 `.github/workflows/release.yml`
  - 在 `npm publish` 之前重新打 tag 或上传修正后的资产
- 仅 npm 打包问题：
  - 仅提升 npm 包版本
  - 保持 `codesmithBinaryVersion` 指向最后一个已知良好的 Rust 发布
  - 重新打包并重新发布包装器
- 一次错误的 npm 发布无法被覆盖：
  - 发布一个修正了元数据或安装逻辑的新 npm 版本
- 发布 tag 的 CNB 镜像失败：
  - 通过 `gh run list --workflow=sync-cnb.yml` 检查运行情况
  - 使用 `gh workflow run sync-cnb.yml` 重新触发，或按照
    [docs/CNB_MIRROR.md](CNB_MIRROR.md#manual-fallback) 手动推送 tag
