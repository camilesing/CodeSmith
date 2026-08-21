# 发布检查清单

v0.8.21/v0.8.22 之间 CHANGELOG 的空档证明我们需要这样一份打标签前的检查
清单。请在发布分支（`work/vX.Y.Z-...`）上的干净 worktree 中按顺序逐步
执行。任何未勾选的框都应视为发布阻塞项。

若需了解底层工具（preflight 脚本、npm 冒烟测试、publish-crates）的更多
背景，参见 [`RELEASE_RUNBOOK.md`](RELEASE_RUNBOOK.md)。

## 1. CHANGELOG 中已存在该版本的条目

- [ ] `CHANGELOG.md` 顶部有 `## [X.Y.Z] - YYYY-MM-DD` 标题
- [ ] 条目应致谢所有实质塑造了本版本的外部贡献者：采集到的 PR 作者、
      关联 issue 的报告者、复现/日志提供者、审查者以及验证协助者。用
      以下命令获取提交列表：
      ```
      git log vPREV..HEAD --no-merges --format="%h %an <%ae> %s" \
        | grep -v '<your-email@…>'
      ```
      对每位贡献者，同时链接其显示名称和（已知时）`@github-handle`。
      然后检查关联 issue 和采集到的 PR，确保报告者/协助者不会仅因没有
      撰写提交而被遗漏。
- [ ] 条目使用 Keep a Changelog 的标题——`Added`、`Changed`、`Fixed`、
      `Security`、`Removed`、`Deprecated`。仅当存在用户必须绕过的实质
      问题时才添加 `Known issues`。
- [ ] 条目把所有被引用的 issue/PR 编号写成 `#NNNN`，以便 GitHub 的自动
      链接器识别。

## 2. 版本号已同步

- [ ] `Cargo.toml` 工作区 `version` 已提升。
- [ ] 各 crate 的 `crates/*/Cargo.toml` 中路径依赖 `version = "..."`
      固定版本都与新的工作区版本一致。
- [ ] `npm/codesmith/package.json` 的 `version` 与 `codesmithBinaryVersion`
      都已提升。
- [ ] `npm/deepseek-tui/package.json` 的 `version` 已为仅保留一个版本的
      弃用 shim 提升。
- [ ] `Cargo.lock` 已刷新（`cargo update --workspace --offline`）。
- [ ] `./scripts/release/check-versions.sh` 报告
      `Version state OK: workspace=X.Y.Z, npm=X.Y.Z, lockfile in sync.`

## 3. Preflight 门禁

在仓库根目录按顺序执行：

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace --all-targets --locked`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
      （在认定某次失败是 flaky 之前，先用
      `cargo test -p PKG --bin BIN -- TEST_NAME` 单独重跑。
      会修改进程级状态——`HOME`、`cwd`、`RUST_LOG`——的测试在并行执行时
      可能发生竞态。将确认的 flake 记入 `Known issues`。）
- [ ] `./scripts/release/publish-crates.sh dry-run`

## 4. npm 包装器冒烟测试

- [ ] `cargo build --release --locked -p codesmith-cli -p codesmith-tui`
- [ ] `node scripts/release/npm-wrapper-smoke.js`
      （如需事后检查临时安装目录，请设置 `DEEPSEEK_TUI_KEEP_SMOKE_DIR=1`。）

## 5. 分支与 PR

- [ ] 分支已推送：`git push -u origin work/vX.Y.Z-...`
- [ ] 已用 `gh pr create --base main --title "chore(release): prepare vX.Y.Z"` 创建 PR
- [ ] PR 描述包含：
  - 一段话概括本次发布主题
  - 上次发布以来新提交的清单
  - 对任何 **Security** 项的明确标注，让审查者一眼看到
  - 贡献者致谢列表
  - CHANGELOG 中的 `Known issues` 块（如有）
- [ ] PR 标题保持**中性**——不要在标题里写 CVE 式措辞或具体攻击细节。
      这些内容留到打标签之后的 GitHub release notes 中。

## 6. CI 通过并完成审查

- [ ] 所有必需的 CI 作业全绿。`versions` 作业应与 preflight 的
      `check-versions.sh` 相互印证，是你的最后一道防线。
- [ ] PR 已经过审查。

## 7. 打标签并发布（审查通过后）

- [ ] `git tag -s vX.Y.Z -m "vX.Y.Z"`
- [ ] `git push origin vX.Y.Z`
- [ ] `release.yml` 工作流已为该标签构建工件并上传到 GitHub release。
- [ ] 线上 GitHub Release 正文拥有独立的 `## Contributors` 或
      `## Credits` 小节；不要只依赖"see CHANGELOG"。用以下命令验证：
      ```
      gh release view vX.Y.Z --repo Hmbown/CodeSmith --json body \
        --jq '.body | test("## (Contributors|Credits)")'
      ```
- [ ] `npm view codesmith@X.Y.Z version codesmithBinaryVersion --json`
      在 npm registry 上显示新版本。
- [ ] `crates.io` 上已有新版本（或 `publish-crates.sh` 作业已推送）。
- [ ] `ghcr.io/hmbown/codesmith:vX.Y.Z` 与 `:latest` 已更新。

## 8. 打标签之后

- [ ] 编辑 GitHub release notes，展开刻意未写入 PR 标题/正文的 CVE 式
      或攻击细节。
- [ ] release 工作流每次重跑后都要重新检查 GitHub Release 正文；工作流
      可能覆盖 notes，意外去掉贡献者致谢。
- [ ] 在下一个发布的跟踪 issue 中记录所有顺延事项。
- [ ] 关闭本次发布修复的所有 issue。

---

如果某一步失败，请**修复根本原因**，而不是跳过它。Pre-commit 钩子、签名
和 CI 都是为了拦住真实问题。`--no-verify`、`--no-gpg-sign` 以及越过审查者
向发布分支强推，按惯例应始终保持硬禁用。
