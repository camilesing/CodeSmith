# CNB Cool 镜像

`cnb.cool/codesmith.net/codesmith` 是本 GitHub 仓库的单向镜像，
服务于 GitHub 访问缓慢或被屏蔽网络（主要是中国大陆）的用户。该
镜像会接收对 `main` 的每次推送、发布工作中使用的每个
`fix/*`、`rebrand/*` 和 `work/v*` 分支、每个 `v*` 发布 tag，以及
Lighthouse/飞书配置使用的腾讯发布候选分支。

## 工作原理

镜像由 [`Sync to CNB`](../.github/workflows/sync-cnb.yml)
GitHub Actions 工作流维护：

- **触发条件：** 推送到 `main`、推送任意 `v*` tag、匹配
  `work/v*` 的发布工作分支、匹配 `fix/*` 和 `rebrand/*` 的
  第一方修复与品牌分支、匹配 `work/v*-feishu-*` 或
  `work/v*-lighthouse*` 的腾讯配置分支，或用于手动恢复的
  `workflow_dispatch`。
- **认证：** 以用户 `cnb` 的 HTTPS basic auth 登录，使用
  `CNB_GIT_TOKEN` 仓库 secret 作为密码。
- **范围：** 只推送触发本次运行的那个 ref。tag 推送只推送该
  tag 本身。分支推送镜像 `main`、第一方 `fix/*`/`rebrand/*`
  分支，或显式匹配的发布/腾讯配置分支。其他功能分支和
  dependabot ref 被*有意*排除在镜像之外。
- **并发：** 运行通过 `cnb-sync` 并发组串行化，因此
  `auto-tag.yml` 紧挨着的 `main` 推送和 tag 推送不会相互竞争。
- **重试：** 每次推送在放弃之前，最多按线性退避（5s、10s）
  重试三次。

CNB 流水线配置同样在 GitHub 中受版本控制，位于
[`/.cnb.yml`](../.cnb.yml)。这是有意为之：同步工作流会将
GitHub ref 强制镜像到 CNB，因此只在 CNB 侧创建的流水线文件会被
覆盖。请通过 GitHub PR 提交 `.cnb.yml` 的更改，让单向镜像将其
带到 CNB。

## CNB tag 发布

当 CNB 收到一个 `v*` tag 时，根目录 `.cnb.yml` 的 tag 流水线会
从源码构建 Linux x64 发布资产，并发布一个包含以下内容的 CNB
release：

- `codesmith-linux-x64`
- `codesmith-tui-linux-x64`
- `codesmith-artifacts-sha256.txt`

这为能访问 CNB 但无法访问 GitHub 的用户提供了一条 CNB 原生的
发布路径。GitHub 仍然是权威的 macOS/Windows 发布矩阵；CNB 的
tag 流水线是对中国友好的 Linux x64 回退方案。

## CNB Linux CI 与发布预检

第一方的 `fix/*` 和 `rebrand/*` 分支会被镜像到 CNB，使重量级的
Linux Rust 门禁运行在腾讯托管的 runner 上，而不是 GitHub
Actions：

- `./scripts/release/check-versions.sh`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --workspace --all-features --locked`
- `cargo build --release --locked -p codesmith-cli -p codesmith-tui`
- `node scripts/release/npm-wrapper-smoke.js`

匹配 `work/v*` 的发布分支还会运行飞书桥接检查和
`./scripts/release/publish-crates.sh dry-run`。GitHub Actions
保留轻量的漂移/fmt 状态，以及 CNB 无法替代的 macOS 和 Windows
作业。

## 发布后验证镜像

当 `release.yml` 针对 `vX.Y.Z` tag 完成后，CNB 镜像应当同时
拥有 `main` 上的新提交和新的 tag：

```bash
# Quick check: does the new tag exist on CNB?
git ls-remote https://cnb.cool/codesmith.net/codesmith.git \
    refs/tags/vX.Y.Z

# Quick check: is CNB's main at the same commit as origin/main?
gh_main=$(git ls-remote https://github.com/Hmbown/CodeSmith.git refs/heads/main | awk '{print $1}')
cnb_main=$(git ls-remote https://cnb.cool/codesmith.net/codesmith.git refs/heads/main | awk '{print $1}')
test "$gh_main" = "$cnb_main" && echo "in sync" || echo "DIVERGED: gh=$gh_main cnb=$cnb_main"
```

或者直接检查工作流运行情况：

```bash
gh run list --workflow=sync-cnb.yml --repo Hmbown/CodeSmith --limit 5
```

如果发布 tag 对应的最近一次运行是 `success`，说明镜像已捕获它。
如果是 `failure`，请按照下面的手动回退方案处理。

## 手动回退

如果工作流因任何原因失败（CNB 限流、token 过期、GitHub 故障
等），维护者可以从其本地检出手动推送到 CNB。这是可行的，因为
CNB token 是一个个人 PAT——工作流使用的同一个 token 就存放在
维护者的密码管理器中。

### 一次性配置

```bash
# Add the CNB remote alongside origin.
git remote add cnb https://cnb:${CNB_TOKEN}@cnb.cool/codesmith.net/codesmith.git

# Or, if you don't want the token in your shell history:
git remote add cnb https://cnb.cool/codesmith.net/codesmith.git
# (you'll be prompted for username `cnb` and password ${CNB_TOKEN}
#  on the first push; subsequent pushes use the credential helper.)
```

### 手动同步一次发布

```bash
# Make sure main is current.
git fetch origin
git checkout main
git reset --hard origin/main

# Push main first, then the tag. Order matters: CNB should see the
# commit before the tag that points at it.
git push cnb main --force-with-lease
git push cnb vX.Y.Z
```

### 手动重新触发工作流

如果工作流本身健康，只是恰好在发布那次运行中失败（例如一次
早已恢复的短暂 CNB 故障），可以在不推送任何内容的情况下重新
触发它：

```bash
gh workflow run sync-cnb.yml --repo Hmbown/CodeSmith
```

`workflow_dispatch` 会针对工作流的默认分支（`main`）运行，
因此这会把当前的 `main` 同步到 CNB。要重新同步某个特定的
tag，请使用上面手动 `git push cnb` 的方式。

## 轮换 `CNB_GIT_TOKEN`

如果工作流开始因认证错误而失败，且 token 已过期：

1. 登录 `cnb.cool`，生成一个具有 `repo`（push）权限的新个人
   访问令牌。
2. 更新 `CNB_GIT_TOKEN` 仓库 secret：
   ```bash
   gh secret set CNB_GIT_TOKEN --repo Hmbown/CodeSmith
   ```
3. 在一个较新的提交上重新触发工作流：
   ```bash
   gh workflow run sync-cnb.yml --repo Hmbown/CodeSmith
   ```
4. 通过 `gh run list --workflow=sync-cnb.yml` 确认运行成功。

## 二进制发布资产与 `codesmith update`

CNB 现在通过受版本控制的 `.cnb.yml` 流水线为 `v*` tag 构建
Linux x64 资产。GitHub 仍然是权威的 macOS/Windows 发布矩阵。
位于 GitHub 被屏蔽网络中的用户应使用以下路径之一：

- **从 CNB 镜像 `cargo install`**：
  ```bash
  cargo install --git https://cnb.cool/codesmith.net/codesmith --tag vX.Y.Z codesmith-cli
  cargo install --git https://cnb.cool/codesmith.net/codesmith --tag vX.Y.Z codesmith-tui
  ```
  （两个二进制文件都是必需的——分发器和 TUI 是分开交付的；
  双二进制安装的原因见 `AGENTS.md`。）

- **CNB release 资产**（Linux x64），前提是匹配的 CNB tag 流水线
  已成功完成。从 `vX.Y.Z` 的 CNB release 下载
  `codesmith-linux-x64`、`codesmith-tui-linux-x64` 和
  `codesmith-artifacts-sha256.txt`，然后根据清单校验二进制文件。

- **`CODESMITH_RELEASE_BASE_URL`** 环境变量，适用于存在发布
  资产 CDN 镜像的情况。npm 包装器安装器和 `codesmith update`
  会读取该变量来重定向二进制下载。对于 `codesmith update`，
  还需设置 `CODESMITH_VERSION=X.Y.Z`，让更新器在不联系
  GitHub 的情况下为镜像发布打上标签。所指向的目录必须包含
  `codesmith-artifacts-sha256.txt` 和各平台二进制文件；格式与
  GitHub Release 资产目录一致。

## 腾讯云 remote-first 路径

Lighthouse + 飞书/Lark 教程将 CNB 用作腾讯侧的源码和自动化
泳道。要获得稳定安装，请从以下地址克隆 `main` 或某个发布
tag：

```bash
https://cnb.cool/codesmith.net/codesmith.git
```

该镜像会接收 `main`、发布 tag，以及 Lighthouse/飞书教程使用的
腾讯配置分支模式。这些 CNB ref 是腾讯侧引导安装的默认源；当
CNB 工作流或凭据不健康时，才回退到 GitHub。

CNB 部署按钮示例位于 `deploy/tencent-lighthouse/cnb/`。在被
复制为 `.cnb.yml` 和 `.cnb/tag_deploy.yml` 之前，它们不会生效，
因为实际运行的部署作业需要 Lighthouse 部署密钥、目标主机以及
显式的 CNB 配额/计费策略。
