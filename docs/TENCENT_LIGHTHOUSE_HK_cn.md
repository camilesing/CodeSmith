# 腾讯云轻量服务器（香港）手机端配置

本手册介绍如何在腾讯云香港的轻量应用服务器实例上搭建一个始终在线的
codesmith 主机，并通过手机上的飞书/Lark 进行控制。

如果你要把它作为腾讯系原生默认路径来教学，请从
[docs/TENCENT_CLOUD_REMOTE_FIRST.md](TENCENT_CLOUD_REMOTE_FIRST.md)
开始。本文件是 Lighthouse 主机本身的实施手册。

## 目标架构

```text
CNB mirror or GitHub branch
  -> /opt/whalebro/codesmith

Feishu/Lark mobile app
  -> Feishu/Lark long-connection bot
  -> codesmith-feishu-bridge systemd service
  -> http://127.0.0.1:7878 codesmith serve --http
  -> /opt/whalebro
       -> codesmith/

Optional public edge:
EdgeOne -> Caddy/Nginx public site on Lighthouse
```

运行时 API 必须保持在 `127.0.0.1`。桥接服务是唯一面向手机的控制
面。EdgeOne 是可选的，只应前置一个经过深思熟虑的公共 HTTP 服务，
而不是运行时 API。

## 远端 Whalebro 工作区

使用 `/opt/whalebro` 作为 VPS 工作区根目录。一等代码检出位于
`/opt/whalebro/codesmith`。

先创建以下路径：

- `/opt/whalebro/codesmith`
- `/opt/whalebro/worktrees`

对于 Rust、Node 和服务相关工作，Linux 足够了。Mac 专属的发布
工作——例如 iOS 模拟器运行、`.app`/DMG 检查、公证（notarization）
和 Apple 签名——仍然属于 Mac。

## Lighthouse 实例

面向出行的推荐套餐：

- 地域：中国香港
- 镜像：纯 Ubuntu 24.04 LTS 或最新 Ubuntu LTS
- 规格：首月购买香港 2 vCPU / 4 GB / 70 GB 套餐
- 登录方式：SSH 密钥，不要用密码
- 防火墙：开放 SSH；运行时 API 仅监听 localhost

腾讯云轻量服务器的官方文档说明 Linux 实例可以使用 SSH 密钥，
且轻量服务器防火墙默认开放 SSH/HTTP/HTTPS。

编译 Rust 并舒适地运行桥接服务，4 GB 内存即可。如果要多代理
并行工作，4 vCPU / 8 GB 套餐更好。

## 飞书 / Lark 应用

在以下地址创建企业自建应用：

- 飞书（中国）：`https://open.feishu.cn/app`
- Lark（国际）：`https://open.larksuite.com/app`

配置步骤：

1. 启用机器人能力。
2. 复制 App ID 和 App Secret。
3. 添加消息收发权限。最简实用集合是：
   - `im:message`
   - `im:message:send_as_bot`
   - 你租户的私信读取权限
   - 仅当你之后有意启用群聊控制时，才添加群 @消息读取权限
4. 添加事件订阅 `im.message.receive_v1`。
5. 使用长连接 / WebSocket 模式。
6. 发布应用并将机器人添加到你的飞书/Lark 会话。

## 服务器初始化

SSH 登录 Lighthouse 实例并运行：

```bash
sudo apt-get update
sudo apt-get install -y git
export CODESMITH_BRANCH=main
export CODESMITH_REPO_URL=https://cnb.cool/codesmith.net/codesmith.git
git clone --branch "$CODESMITH_BRANCH" "$CODESMITH_REPO_URL" /tmp/codesmith
cd /tmp/codesmith
sudo CODESMITH_REPO_URL="$CODESMITH_REPO_URL" \
  CODESMITH_REPO_BRANCH="$CODESMITH_BRANCH" \
  bash scripts/tencent-lighthouse/bootstrap-ubuntu.sh
```

如果需要从 VPS 获得 push 权限，请改用 SSH 仓库 URL。如果 CNB
镜像不可用，回退到：

```bash
export CODESMITH_REPO_URL=https://github.com/Hmbown/CodeSmith.git
```

对于稳定的发布文档，在使用前请确认 CNB 镜像已有所需的分支或
tag：

```bash
export CODESMITH_REPO_URL=https://cnb.cool/codesmith.net/codesmith.git
git ls-remote "$CODESMITH_REPO_URL" \
  refs/heads/main \
  refs/tags/v0.8.37
```

CNB 镜像会接收 `main` 和发布 tag。在这条 Lighthouse 路径中，CNB
是默认源；只有当 CNB 工作流或凭据不健康时，才回退到 GitHub。

如果这套部署配置尚未推送到 Git，请先推送分支，或在运行这些
命令之前把这份检出台账复制到 VPS。全新克隆的 VPS 看不到未提交的
本地文件。

为 `codesmith` 用户安装 Rust 1.88+，然后构建两个交付的二进制
文件：

```bash
sudo -iu codesmith
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
sed -n '1,120p' /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --profile minimal
. "$HOME/.cargo/env"
rustup default stable
cd /opt/whalebro/codesmith
cargo install --path crates/cli --locked --force
cargo install --path crates/tui --locked --force
exit
```

复制并安装桥接/服务文件：

```bash
cd /opt/whalebro/codesmith
sudo bash scripts/tencent-lighthouse/install-services.sh
```

编辑完两个 env 文件后，验证桥接/运行时的配对关系：

```bash
sudo -u codesmith node /opt/codesmith/bridge/scripts/validate-config.mjs \
  --env /etc/codesmith/feishu-bridge.env \
  --runtime-env /etc/codesmith/runtime.env \
  --workspace-root /opt/whalebro \
  --check-filesystem
```

## 机密信息

生成一个运行时令牌，并将相同的值写入两个 env 文件：

```bash
openssl rand -hex 32
sudoedit /etc/codesmith/runtime.env
sudoedit /etc/codesmith/feishu-bridge.env
```

必需的值：

- `/etc/codesmith/runtime.env`
  - `DEEPSEEK_API_KEY`
  - `CODESMITH_RUNTIME_TOKEN`
- `/etc/codesmith/feishu-bridge.env`
  - `FEISHU_APP_ID`
  - `FEISHU_APP_SECRET`
  - 飞书设为 `FEISHU_DOMAIN=feishu`，Lark 设为 `lark`
  - `CODESMITH_RUNTIME_TOKEN`
  - 首次部署设为 `FEISHU_ALLOW_GROUPS=false`

首次配对时，二选一：

1. 临时设置 `CODESMITH_ALLOW_UNLISTED=true`，给机器人发消息，复制
   返回的 `chat_id`，然后设置 `CODESMITH_CHAT_ALLOWLIST=<chat_id>`
   并重新关闭未列名单访问。
2. 或者从飞书/Lark 事件日志获取 chat ID，并在首次启动之前设置
   白名单。

## 启动服务

```bash
sudo systemctl start codesmith-runtime
sudo systemctl status codesmith-runtime --no-pager
curl -s http://127.0.0.1:7878/health

sudo systemctl start codesmith-feishu-bridge
sudo journalctl -u codesmith-feishu-bridge -f
```

两个服务都配置完成后，运行 Lighthouse 诊断（doctor）：

```bash
cd /opt/whalebro/codesmith
sudo bash scripts/tencent-lighthouse/doctor.sh
```

开机自启由 `install-services.sh` 完成；如有需要：

```bash
sudo systemctl enable codesmith-runtime codesmith-feishu-bridge
```

## 手机端命令

私信（DM）可以是纯文本，这是预期的首选控制路径：

```text
check git status and summarize what needs attention
```

群聊默认禁用。如果之后设置了 `FEISHU_ALLOW_GROUPS=true`，群内
提示词必须以 `/ds` 开头。

常用命令：

- `/status`
- `/threads`
- `/new`
- `/resume <thread_id>`
- `/interrupt`
- `/compact`
- `/allow <approval_id>`
- `/deny <approval_id>`
- `/allow <approval_id> remember`

只有当你有意让运行时线程在未来的工具调用上转向自动批准时，才使用
`remember`。

## CNB 部署按钮

手动 Lighthouse 配置跑通之后，CNB 可以成为可复现的部署按钮：

1. 将 `deploy/tencent-lighthouse/cnb/cnb.yml.example` 复制为 CNB
   仓库中的 `.cnb.yml`。
2. 将 `deploy/tencent-lighthouse/cnb/tag_deploy.yml.example` 复制为
   `.cnb/tag_deploy.yml`。
3. 配置 `deploy/tencent-lighthouse/cnb/README.md` 中记录的 CNB
   部署机密。
4. 触发 `lighthouse-hk` 部署环境。

在服务器变得"无聊"（稳定）之前，请保持手动操作。每次 push 自动
部署之后会很方便，但它们会消耗 CNB 配额，并可能在手机回合进行
期间重启桥接服务。

## EdgeOne

首次搭建飞书/Lark 长连接不需要 EdgeOne。仅当你需要在 Lighthouse
主机上某个经过深思熟虑的公共服务前面放置公共 HTTPS 域名时，才
添加它。

EdgeOne 的良好用途：

- 公共文档或教程站点
- 小型运维状态页
- 未来的 webhook 模式桥接端点
- 托管在同一 Lighthouse 实例上的演示 Web 应用

不要使用 EdgeOne 暴露：

- `http://127.0.0.1:7878`
- `/v1/*` 运行时端点
- 任何接受 `CODESMITH_RUNTIME_TOKEN` 的端点

## 端到端验证

从手机向机器人发私信：

1. 发送 `/status`，确认运行时版本、localhost 绑定、认证状态、
   工作区、git 仓库、分支和脏文件计数。
2. 发送一个无害的提示词，例如 `summarize git status`。
3. 在回合进行中发送 `/interrupt`，确认回合停止。
4. 发送 `/threads`，然后对某个列出的线程执行
   `/resume <thread_id>`。
5. 触发一次工具审批，并验证 `/allow <approval_id>` 和
   `/deny <approval_id>` 两条路径。
6. 重启两个服务并再次运行 `/status`。
7. 重启实例，然后确认 `systemctl status codesmith-runtime` 和
   `systemctl status codesmith-feishu-bridge` 恢复为 active。

## 运维注意事项

- 将 `codesmith serve --http` 绑定到 `127.0.0.1`。
- 在本套配置中，保持 Lighthouse 防火墙只聚焦于 SSH。
- 使用 SSH 密钥认证。
- 从 Blink/Termius 进行应急终端操作时使用 `tmux`。
- 通过手机工作时，让 `/opt/whalebro/codesmith` 保持在个人分支上。
