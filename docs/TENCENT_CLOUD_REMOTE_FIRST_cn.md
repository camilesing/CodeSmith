# 腾讯云远程优先快速开始

这是一条有明确倾向的腾讯系教学路径，面向想要常驻在线的智能体工作区、手机控制界面，以及一套在中国大陆运行良好的技术栈的 codesmith 用户。

它是对本地安装路径的补充。如果你只想在笔记本电脑上使用 `codesmith`，请从 README 快速开始入手。如果你想要“一个可以用手机控制的 CodeSmith 远程工作台”，请从这里开始。

## 默认技术栈

```text
GitHub main/tags
  -> CNB mirror: cnb.cool/codesmith.net/codesmith
  -> optional CNB build/deploy pipeline
  -> Tencent Lighthouse HK
       /opt/whalebro/codesmith
       /opt/whalebro/worktrees
       codesmith-runtime.service on 127.0.0.1:7878
       codesmith-feishu-bridge.service
  -> Feishu/Lark phone DM

EdgeOne is optional:
  public HTTPS domain -> EdgeOne -> Caddy/Nginx on Lighthouse
```

## 各组件的作用

- **CNB** 是腾讯侧的源码与自动化通道。当 GitHub 克隆和按 tag 安装很慢时，现有的
  `cnb.cool` 镜像很有用。可选的 CNB 部署模板位于
  `deploy/tencent-lighthouse/cnb/`。
- **Lighthouse** 是私有的常驻主机。它拥有 `/opt/whalebro`、systemd、Rust/Node
  安装以及 `codesmith serve --http` 运行时。
- **飞书/Lark（Feishu/Lark）** 是第一手机 UI。桥接使用长连接模式，因此首次设置
  不需要公开的 webhook URL。
- **EdgeOne** 只是公开边缘，仅当你有意暴露 web 面（如文档、状态页或未来的
  webhook 端点）时才使用。不要把运行时 API 放到 EdgeOne 后面。

## 第一课：让远程智能体跑起来

1. 购买或复用一台位于香港的 Tencent Lighthouse 实例。
2. 默认情况下，当分支或 tag 在 CNB 上存在时从 CNB 克隆：

   ```bash
   export DEEPSEEK_REPO_URL=https://cnb.cool/codesmith.net/codesmith.git
   git ls-remote "$DEEPSEEK_REPO_URL" refs/heads/main
   ```

   与 `work/v*-feishu-*` 或 `work/v*-lighthouse*` 匹配的 Tencent 设置分支由
   GitHub 的 CNB 同步工作流做镜像。仅当 CNB 工作流或凭据不健康时才使用
   GitHub URL。

3. 在服务器上引导 `/opt/whalebro`：

   ```bash
   export DEEPSEEK_BRANCH=main
   git clone --branch "$DEEPSEEK_BRANCH" "$DEEPSEEK_REPO_URL" /tmp/codesmith
   cd /tmp/codesmith
   sudo DEEPSEEK_REPO_URL="$DEEPSEEK_REPO_URL" \
     DEEPSEEK_REPO_BRANCH="$DEEPSEEK_BRANCH" \
     bash scripts/tencent-lighthouse/bootstrap-ubuntu.sh
   ```

4. 为 `codesmith` 用户安装 Rust，构建两个二进制，并按照
   `docs/TENCENT_LIGHTHOUSE_HK.md` 安装 systemd 单元。
5. 配置一个飞书/Lark 自建应用，填写 `/etc/deepseek/feishu-bridge.env`，
   先运行校验器，再运行 VPS doctor。
6. 在手机私聊（DM）中验证 `/status`、一条无害的提示、`/interrupt`、
   `/threads`、`/resume`、审批允许/拒绝、服务重启以及重启后的持久性。

## 第二课：把 CNB 变成部署按钮

手动 Lighthouse 路径跑通之后，把 `deploy/tencent-lighthouse/cnb/` 中未启用的
示例复制到 CNB 仓库：

- `cnb.yml.example` -> `.cnb.yml`
- `tag_deploy.yml.example` -> `.cnb/tag_deploy.yml`

预期的部署按钮应当：

1. 运行桥接校验/测试和轻量的发布版本检查。
2. 使用存储为 CNB secret 的部署密钥 SSH 到 Lighthouse。
3. 更新 `/opt/whalebro/codesmith`。
4. 重新构建/安装两个二进制。
5. 重新安装/重启 systemd 服务。
6. 运行 `scripts/tencent-lighthouse/doctor.sh`。

在部署密钥、目标主机、计费/配额和回滚策略都明确之前，不要在 `main` 上
启用它。

## 第三课：仅为公开 HTTPS 添加 EdgeOne

飞书/Lark 长连接桥接无需 EdgeOne 即可工作。当你想在一个深思熟虑的 HTTP 服务
前面加一个公开域名时，再添加 EdgeOne：

- 公开的教程/文档站点
- 一个小型运维状态页
- 未来的 webhook 模式桥接
- 运行在同一 Lighthouse 源站上的演示应用

始终遵守这些规则：

- `codesmith serve --http` 保持绑定在 `127.0.0.1`。
- `/v1/*` 运行时端点永不公开。
- `DEEPSEEK_RUNTIME_TOKEN` 绝不离开服务器 env 文件。
- 在设置具体的群白名单之前，飞书/Lark 群控制保持关闭。
- 除非维护者明确接受风险，手机桥接的自动批准保持关闭。

## 讲解顺序

向新的远程优先用户介绍 codesmith 时，使用这个顺序：

1. **本地心智模型**：`codesmith` 是调度器，`codesmith-tui` 是配套运行时，
   两个二进制都重要。
2. **智能体安全**：Plan/Agent/YOLO 与审批模式和沙箱相互独立。
3. **远程运行时**：`codesmith serve --http` 是 localhost 运行时 API，不是
   公开 web 应用。
4. **手机桥接**：飞书/Lark 消息通过白名单桥接变成运行时请求。
5. **CNB 自动化**：手动设置得到验证后，CNB 把这套设置变成可重复的部署
   按钮。
6. **EdgeOne 边缘**：在确切知道要暴露什么公开面之后，再添加公开边缘。

## 参考

- CNB 镜像详情：`docs/CNB_MIRROR.md`
- Lighthouse 实施手册：`docs/TENCENT_LIGHTHOUSE_HK.md`
- 飞书/Lark 桥接：`integrations/feishu-bridge/README.md`
- CNB 模板：`deploy/tencent-lighthouse/cnb/`
