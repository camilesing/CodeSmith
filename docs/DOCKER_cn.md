# Docker

CodeSmith 会为每个发布版本向 GitHub Container Registry 发布多架构
Linux 镜像。

```bash
docker pull ghcr.io/camilesing/codesmith:latest
```

## 快速开始

使用 Docker 管理的数据卷运行已发布的镜像：

```bash
docker volume create codesmith-home

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:latest
```

使用固定的发布 tag 以获得可复现的安装：

```bash
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/camilesing/codesmith:vX.Y.Z
```

将 `vX.Y.Z` 替换为
[GitHub Releases](https://github.com/camilesing/CodeSmith/releases)
中的某个 tag。

## 默认镜像契约

`ghcr.io/camilesing/codesmith:latest` 和语义化版本 tag 是保守的运行时
镜像：

- 容器以非 root 的 `codesmith` 用户运行，UID/GID 为 `1000:1000`
- 镜像不提供免密 `sudo`
- 镜像的设计用途是让 CodeSmith 在挂载的工作区上运行，而不是在
  运行时修改基础操作系统
- 用户状态存放在挂载于 `/home/codesmith/.codesmith` 的卷中

这个默认设定是有意为之。继续使用它可获得最小的信任边界。如果
某个项目需要在 Docker 内使用 `apt-get`、编译工具链、Node/Python
包管理器、自定义 CA 证书或其他类似宿主机的环境，请构建一个显式
的 toolbox 镜像，而不是更改默认镜像契约。

## 可选开启的 toolbox/自定义镜像

仓库包含一个示例
[`docs/examples/Dockerfile.toolbox`](examples/Dockerfile.toolbox)，
它在官方镜像基础上扩展了免密 `sudo` 和常用开发包。当你想要可
复现的项目环境时，使用固定的 CodeSmith tag 构建它：

```bash
docker build -f docs/examples/Dockerfile.toolbox \
  --build-arg CODESMITH_IMAGE=ghcr.io/camilesing/codesmith:vX.Y.Z \
  --build-arg TOOLBOX_PACKAGES="git openssh-client curl build-essential pkg-config python3 python3-pip nodejs npm" \
  -t codesmith-toolbox:my-project .
```

仅在一次性测试中使用 `latest`。对于共享项目，请保持
`CODESMITH_IMAGE` 值固定，并像审查其他开发环境变更一样审查新增
的包。

使用相同的工作区和状态挂载运行 toolbox 镜像：

```bash
docker volume create codesmith-my-project-home

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-my-project-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  codesmith-toolbox:my-project
```

在这个可选开启的镜像内，CodeSmith 可以使用诸如
`sudo apt-get update` 和 `sudo apt-get install -y <package>` 之类的
命令。为了容器的可复现性，请优先将这些包直接固化到 toolbox
Dockerfile 中，而不是让长期存活的容器随意漂移。

不要把 API 密钥、SSH 私钥或其他机密信息固化到自定义镜像中。
在运行时传入 API 密钥，并有意识地挂载任何 SSH 材料，最好以只读
方式且仅限于需要它的项目。

### Compose toolbox 模板

如果你更倾向于可复现的 `docker compose` 入口，请使用
[`docs/examples/compose.toolbox.yml`](examples/compose.toolbox.yml)。
它基于 [`docs/examples/Dockerfile.toolbox`](examples/Dockerfile.toolbox)
构建 toolbox 镜像，并让项目状态卷保持显式：

```bash
CODESMITH_IMAGE=ghcr.io/camilesing/codesmith:vX.Y.Z \
CODESMITH_TOOLBOX_IMAGE=codesmith-toolbox:my-project \
CODESMITH_HOME_VOLUME=codesmith-my-project-home \
CODESMITH_WORKSPACE="$PWD" \
docker compose -f docs/examples/compose.toolbox.yml run --rm codesmith
```

为每个需要独立工具链或独立 `.codesmith` 状态的项目使用不同的
`CODESMITH_TOOLBOX_IMAGE` 和 `CODESMITH_HOME_VOLUME`。该 Compose
文件还展示了 SSH 材料和本地 CA 证书的可选只读挂载；除非项目需要，
否则保持这些配置被注释掉的状态。

## 多个相互独立的项目

为每个项目使用一个具名状态卷，这样会话、配置、技能、记忆和
离线队列就不会在工作区之间串扰：

```bash
project="$(basename "$PWD")"
image="codesmith-toolbox:${project}"
docker volume create "codesmith-${project}-home"

docker run --rm -it \
  --name "codesmith-${project}" \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v "codesmith-${project}-home:/home/codesmith/.codesmith" \
  -v "$PWD:/workspace" \
  -w /workspace \
  "$image"
```

对于工具链不同的项目，构建不同的 toolbox tag，例如
`codesmith-toolbox:frontend` 和 `codesmith-toolbox:backend`。Issue
#2217 中讨论的独立启动器想法可以建立在这一契约之上，但它有意
不放在核心 Docker 镜像的范围内。

## 项目引导脚本

CodeSmith 不会自动执行 `.codesmith/setup.sh` 或遗留的
`.deepseek/setup.sh`。如果你将其中一个文件保留为本地项目配方，
请显式运行它。对于团队共享的环境搭建，优先使用提交到仓库的项目
脚本或 toolbox Dockerfile，这样环境才可以被审查和重建。

例如，在启动 CodeSmith 之前运行一个已提交的引导脚本：

```bash
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-my-project-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  --entrypoint bash \
  codesmith-toolbox:my-project \
  -lc './scripts/bootstrap-dev.sh && exec codesmith'
```

对需要 `sudo` 的引导脚本使用 toolbox 镜像。默认镜像不会进行
提权。

## 自定义 CA 证书与代理

对于企业代理、dev-sidecar 或自签名的内部服务，优先将受信任的
CA 证书固化到自定义 toolbox 镜像中：

```dockerfile
USER root
COPY docker/certs/*.crt /usr/local/share/ca-certificates/
RUN update-ca-certificates
USER codesmith
```

所有复制到 `/usr/local/share/ca-certificates/` 的文件必须使用
`.crt` 扩展名。不要让私有 CA 材料进入公开镜像。

对于仅本地运行的场景，可以以只读方式挂载证书并在容器启动时
更新信任库：

```bash
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-my-project-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -v "$PWD/docker/certs:/usr/local/share/ca-certificates/local:ro" \
  -w /workspace \
  --entrypoint bash \
  codesmith-toolbox:my-project \
  -lc 'sudo update-ca-certificates && exec codesmith'
```

这套 CA 工作流需要可选开启的 toolbox 镜像，因为默认镜像不包含
免密 `sudo`。

## 本地构建

从代码检出本地构建镜像：

```bash
docker build -t codesmith .
```

然后使用相同的 Docker 管理数据卷运行它：

```bash
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  codesmith
```

Docker Hub 发布未配置；GHCR 是受支持的预构建镜像
仓库。

## 环境变量

| 变量                 | 必需     | 描述                                            |
|----------------------|----------|--------------------------------------------------|
| `CODESMITH_API_KEY`   | 是      | Provider API 密钥（默认 DeepSeek）                                |
| `CODESMITH_BASE_URL`   | 否      | 自定义 API base URL（例如 `https://api.deepseek.com`） |
| `NO_COLOR`            | 否      | 设置为 `1` 以禁用终端彩色输出                    |

## 卷

挂载 `/home/codesmith/.codesmith` 以在容器重启之间持久化会话、
配置、技能、记忆和离线队列。Docker 管理的具名卷是
最安全的默认选择，因为 Docker 创建它时使用的所有权正是容器
可写的：

```bash
-v codesmith-home:/home/codesmith/.codesmith
```

不挂载此卷时，容器每次都会全新启动。

如果你改为绑定挂载一个已存在的宿主机目录，镜像会以非 root 的
`codesmith` 用户（UID/GID 为 `1000:1000`）运行。被挂载的目录必须
可被该用户写入，否则在 `.codesmith/tasks` 下创建运行时目录时
启动可能失败。在 Linux 宿主机上，要么使用上面的具名卷，要么
显式准备绑定挂载：

```bash
mkdir -p ~/.codesmith
sudo chown -R 1000:1000 ~/.codesmith

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v ~/.codesmith:/home/codesmith/.codesmith \
  ghcr.io/camilesing/codesmith:latest
```

这条 `chown` 会更改宿主机 `~/.codesmith` 目录的所有权。如果你
不希望容器 UID 拥有你的本地配置，请跳过它，改用具名卷。

## 非交互 / 流水线用法

当 stdin 不是 TTY 时，`codesmith` 会进入分发器的一次性模式
（`codesmith -c "…"`）。通过 stdin 管道传入提示词：

```bash
echo "Explain the Cargo.toml in structured English." | \
  docker run --rm -i -e CODESMITH_API_KEY ghcr.io/camilesing/codesmith:latest
```

## 本地构建

```bash
# Single platform (your host architecture)
docker build -t codesmith .

# Multi-platform (requires a builder with emulation)
docker buildx create --use
docker buildx build --platform linux/amd64,linux/arm64 -t codesmith .
```

## Devcontainer

仓库包含面向 VS Code / GitHub Codespaces 的
[`.devcontainer/devcontainer.json`](../.devcontainer/devcontainer.json)
配置。它预装了 Rust 工具链、rust-analyzer 和 `codesmith` 二进制
文件。在 devcontainer 中打开仓库即可获得开箱即用的开发环境。

## 发布状态

Docker 镜像发布是发布门禁的一部分。镜像会以语义化版本 tag 加
`latest` 发布到 GHCR，覆盖 `linux/amd64` 和 `linux/arm64`。
