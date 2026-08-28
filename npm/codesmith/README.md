# codesmith

Install and run CodeSmith, the agentic terminal for open-source and open-weight coding
models, from GitHub release artifacts.


## Install

```bash
npm install -g codesmith
# or
pnpm add -g codesmith
```

For project-local usage:

```bash
npm install codesmith
npx codesmith --help
```

`postinstall` tries to download platform binaries into `bin/downloads/` and
exposes `codesmith` and `codesmith-tui` commands. If GitHub release assets are
temporarily unreachable, install continues and the wrapper retries the download
on first run.

## First run

```bash
codesmith login --api-key "YOUR_DEEPSEEK_API_KEY"
codesmith doctor
codesmith
```

The `codesmith` facade and `codesmith-tui` binary share
`~/.codesmith/config.toml` for DeepSeek auth and default model settings. Legacy
`~/.deepseek/` installs are no longer read; migrate the directory to
`~/.codesmith/`.
Common TUI commands are available directly through the facade, including
`codesmith doctor`, `codesmith models`, `codesmith sessions`, and
`codesmith resume --last`.

The app talks to DeepSeek's documented OpenAI-compatible Chat Completions API.
Set `CODESMITH_BASE_URL` only if you need the China endpoint or DeepSeek beta
features such as strict tool mode, chat prefix completion, or FIM completion.

NVIDIA NIM-hosted DeepSeek V4 Pro is also supported:

```bash
codesmith auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"
codesmith --provider nvidia-nim
```

For a single process, set `CODESMITH_PROVIDER=nvidia-nim` and `NVIDIA_API_KEY`
or `NVIDIA_NIM_API_KEY` (with `CODESMITH_API_KEY` / `DEEPSEEK_API_KEY` as a
compatibility fallback).
The NIM default model is `deepseek-ai/deepseek-v4-pro` and the default base URL
is `https://integrate.api.nvidia.com/v1`. With `--provider nvidia-nim`,
`--model deepseek-v4-flash` maps to `deepseek-ai/deepseek-v4-flash`.

## Supported platforms

Prebuilt binaries for the GitHub release are downloaded automatically:

- Linux x64
- Linux arm64 (v0.8.8+)
- macOS x64 / arm64
- Windows x64

Other platform/architecture combinations (musl, riscv64, FreeBSD, …) aren't
shipped as prebuilts. Unsupported platforms, checksum failures, and glibc
compatibility problems still fail with a clear error pointing you at
`cargo install codesmith-cli codesmith-tui --locked` and the full
[docs/INSTALL.md](https://github.com/camilesing/CodeSmith/blob/main/docs/INSTALL.md)
build-from-source guide.

## Configuration

- Default binary version comes from `codesmithBinaryVersion` in `package.json`
  (with `deepseekBinaryVersion` as a backward-compat fallback).
- Set `CODESMITH_VERSION` to override the release version.
- Set `CODESMITH_GITHUB_REPO` to override the source repo (defaults to `camilesing/CodeSmith`).
- Set `CODESMITH_RELEASE_BASE_URL` to use an internal or mirrored
  release-asset directory when GitHub Releases is unavailable. The directory
  must contain `codesmith-artifacts-sha256.txt` and the platform binaries.
- Set `CODESMITH_FORCE_DOWNLOAD=1` to force download even when the cached binary is already present.
- Set `CODESMITH_DISABLE_INSTALL=1` to skip install-time download.
- Set `CODESMITH_OPTIONAL_INSTALL=1` to make install-time retryable download
  failures warn and exit `0` instead of failing `npm install`.

## Release integrity

- `npm publish` runs a release-asset check to ensure all required binary assets
  exist for the target GitHub release before publishing.
- Install-time downloads are verified against the release checksum manifest before
  the wrapper marks them executable.
