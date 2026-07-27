# §F5c — Dylib INSTALL 侧 (phase 2 续作·下) Design

- **Date:** 2026-07-23
- **Branch:** `feat/pluggable-framework-core`
- **Predecessor:** §F5b (dylib LOAD 侧), commits `6891d605`→`93240b74` (T1 `manifest.rs` → T7 docs)
- **Spec:** this file
- **Plan (to be written):** `docs/superpowers/plans/2026-07-23-codesmith-extension-system-slice-5c.md`
- **Authoritative scope source:** `ROADMAP.md:2598-2604` (§F5b "By-design gaps §F5c") + `docs/EXTENSIONS.md` intro §F5c sentence

---

## 1. Overview / 目标

§F5b 落地了 dylib **LOAD** 半：`libloading` loader + `extension.toml` manifest +
三形态发现 + 项目本地 trust gate (Model A) + reload wiring + cdylib fixture。§F5c
落地 **INSTALL** 半：把一个扩展源（Git 仓库 / 本地路径）**fetch → build 成 cdylib
→ place 到 extensions root → 写 `extension.toml` + `installed[]` provenance**，使下一
次 `discover_dylib` 能发现并 `ExtensionRunner::load_dylib` 加载。

`/extension install`/`uninstall` 从 stub（`extension_commands.rs:201`/`:207`）变为真
实现。本切片 **不改** §F5b 的 load/discovery/trust-gate 机制（已稳定 + 测过），只在
其上游加"如何把 dylib 弄到 disk 上"。

> Slice 边界：§F5b = "loads dylibs from disk"；§F5c = "fetch/build/place dylibs onto
> disk"。两者合起来 = 完整 phase-2 dylib 机器的 install→load 闭环。

## 2. Background — §F5b 真实 API（§F5c 对齐 + 复用的 shape）

以代码为准（§F5b plan 草稿有 API drift，不照抄）。

### install-source traits（slice 1 已落地 trait，§F5c 只加 impl，不改 trait 形状）
`crates/extensions/src/install_source.rs`：
```rust
pub struct SourceArtifact { pub path: PathBuf, pub provenance: String }
pub trait ExtensionSource:  Send + Sync { fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError>; }
pub trait ExtensionBuilder:  Send + Sync { fn build(&self, src_dir: &Path) -> Result<PathBuf, ExtensionError>; }
pub trait ExtensionPlacer:   Send + Sync { fn place(&self, artifact: &Path) -> Result<PathBuf, ExtensionError>; }
pub struct UnimplementedSource; // 永远 Err(Install) —— §F5c 复用为 crate/prebuilt stub
```

### loader / runner（§F5c 复用，不改）
`crates/extensions/src/loader.rs:31`：
```rust
pub fn load_dylib(path: &Path) -> Result<(Library, Box<dyn Extension>), ExtensionError>
```
`crates/extensions/src/runner.rs`：
```rust
libraries: Mutex<Vec<Library>>,                       // :112, reload 不清 (Q1)
pub async fn load_dylib(&self, path: &Path) -> Result<(), ExtensionError>  // :179
pub async fn load(&self, ext: &dyn Extension) -> Result<(), ExtensionError> // :166
```
`Extension::metadata() -> &ExtensionMetadata`（含 `id` + `version`）—— §F5c D8 用它取
权威 id/version。

### discovery（§F5c 复用，不改签名；仅修 1 处 stale 注释，见 §7）
`crates/extensions/src/discovery.rs`：
```rust
pub fn discover_dylib(global_roots: &[PathBuf], project_roots: &[PathBuf]) -> Vec<DiscoveredSource>  // :66, 2-arg
pub struct DiscoveredSource { id, version, config_path: Option<PathBuf>, dylib_path: PathBuf, global: bool }  // flat
pub fn apply_trust_gate(sources: Vec<DiscoveredSource>, trust_untrusted: bool) -> Vec<DiscoveredSource>  // :183
fn default_dylib_filename(id: &str) -> String  // :50, PRIVATE —— §F5c 改 pub(crate) 复用
```

### manifest（§F5c 写，不改 struct）
`crates/extensions/src/manifest.rs`：
```rust
pub struct ExtensionManifest { id, version, entry: Option<String>, source: Option<ManifestSource>, api_version: Option<String> }
pub struct ManifestSource { kind: String /* "type" */, ref_: Option<String> /* "ref" */ }
```

### state（§F5c 改 `installed` 字段 + 加 mutator）
`crates/tui/src/extension_state.rs`：
```rust
pub struct ExtensionStateStore { path: Option<PathBuf>, disabled: BTreeSet<String>, installed: BTreeSet<String> }  // :33
struct OnDiskState { disabled: Vec<String>, installed: Vec<String> }  // :42
pub fn installed() -> Vec<String>  // :106, reader（无 mutator —— §F5c 加）
```

### fixture / engine（§F5c 测试 + install 命令复用）
- `CODESMITH_FIXTURE_DYLIB` env（§F5b T4 `build.rs` 发）—— FakeBuilder e2e 返回它。
- `populate_extension_runtime`（`engine.rs:378`）/ `reload_extension_runtime`（`:484`）
  —— 不改；install 不进 agent loop。
- `crate::config::is_workspace_trusted(workspace)` / `effective_home_dir()` —— install
  warn + global root 复用。

## 3. Scope

### In scope
1. `SourceSpec` parser（prefix 语法 `git:`/`path:`/`crate:`/`prebuilt:` + `--global` flag）。
2. `GitSource` + `LocalPathSource`（`ExtensionSource` impl，must-have）。
3. `CargoBuilder`（`ExtensionBuilder` impl，shell out `cargo build --release --locked`）。
4. `Placer`（`ExtensionPlacer` impl，拷 dylib + 复用 `default_dylib_filename`）。
5. `Installer` orchestrator（trait-DI：`&dyn Source`/`Builder`/`Placer` + `&mut StateStore`）。
6. `installed` 字段 `BTreeSet<String>` → `BTreeMap<String, String>`（id→provenance）+
   mutators `add_installed`/`remove_installed`/`provenance_for`/`installed_ids`。
7. `extension.toml` 写（Installer 在 dylib 旁写 id/version/entry/source）。
8. `/extension install`/`uninstall` 真实现（替换 stub）。
9. D8：install 时临时 `loader::load_dylib` 读 `metadata()` 取权威 id/version。
10. install→load e2e（FakeBuilder + §F5b fixture）+ CargoBuilder throwaway-crate 单测。

### Out of scope（explicitly deferred）
- **`CratesIoSource` / `PrebuiltDylibSource`** —— stub（`UnimplementedSource`-style，返回
  `ExtensionError::Install("§F5c-later: <kind> source not yet implemented")`）。ROADMAP 标
  nice-to-have；CratesIo 需 crates.io registry HTTP+version+checksum，Prebuilt 需 HTTP
  fetch+sha256 verify，各 ~1 task + 新 dep —— 推后。
- **`clear_tools`/`clear_commands` + Library 真卸载** —— Q1 接受 bounded 留存（uninstall
  删文件+state，已加载 Library/tools/commands 留到进程重启保 sound）。
- **`settings.extensions[]` configured-paths** —— ROADMAP §F5c 未列；`discover_dylib`
  保持 2-arg（§F5b plan 草稿的 3-arg configured 是 drift）。
- **tui-level dylib e2e**（`run_tui` 触发 install/discover/reload）—— §F5 precedent
  （`EngineHost`+`run_tui`+真信任 fixture 比例失衡）。
- **abi_stable** —— 永不（§2.4）。
- **`/extension install` 的依赖锁/特征选择 UI** —— 默认 `--locked`、默认 features；
  无 `--features`/`--offline` flag（YAGNI）。

## 4. Architecture — `Installer` orchestrator + trait-DI

```
/extension install <spec> [--global]
        │
        ▼
SourceSpec::parse(arg) ──► (kind, body, ref_opt, scope)
        │   kind∈{git,path,crate,prebuilt}
        │   crate/prebuilt ──► UnimplementedSource stub → Err
        ▼
construct GitSource{url,ref} | LocalPathSource{dir} | UnimplementedSource
        ▼
Installer { source: &dyn ExtensionSource,     // 真产=Git/Local/Unimpl, 测=FakeSource
            builder: &dyn ExtensionBuilder,   // 真产=CargoBuilder, 测=FakeBuilder
            state:   &mut ExtensionStateStore,
            scope:   InstallScope }
        ▼
install():  fetch → build → [D8 temp-load metadata → id,version]
            → 构造 Placer{id,scope}（id 来自 D8，故 Placer 在 install() 内构造、非注入）
            → place → 写 manifest → add_installed → (untrusted? warn)
uninstall(id):  locate <root>/<id>/ → rm → remove_installed → warn(bounded retention)
```

trait-DI 让 `Installer` 的核心逻辑可注入 fake——但**只对 source+builder**（e2e 要 fake
的部分）。`Placer` 在 `install()` 内部按 D8 的 id 构造（id 在 build 后才知，故 Placer 不能
像 source/builder 那样在 Installer 构造前注入）；e2e 因此用**真 Placer** 练 place+manifest
写。e2e 注入 `FakeSource`+`FakeBuilder`（避开真 fetch + 真 `cargo build` 的 target-dir
lock + 全量依赖树重建），真 `CargoBuilder` 在独立 throwaway crate 上单测。

## 5. Key design decisions（7 问 + D8）

### Q1 — 卸载范围 = bounded 留存（不实现真卸载）
- **决策**：§F5c 不加 `clear_tools`/`clear_commands`、不 drop `Library`。uninstall =
  删 `<root>/<id>/` 文件 + `remove_installed(id)`。uninstall+`/extension reload` 后
  uninstalled ext 的 **handlers 消失**（`clear_handlers` 清空 + 不再 re-discover），但
  **tools/commands 留在 `tools`/`commands` HashMap 到进程重启**（无 clear_tools，未被
  覆写）；`Library` 留存保 vtable 存活 = sound。
- **理由**：真卸载有 soundness 风险——host `ToolRegistry` 缓存的 `Arc<dyn ToolDefinition>`
  clone 仍引用 vtable，中途 drop `Library` 后调该 tool 会 segfault；且 `tools` HashMap 按
  name 键无 source 归属，选择性清需额外 tool→dylib 映射。ROADMAP 括注"Q1 接受 bounded
  留存保底正确性"即此意。YAGNI。
- **rejected**：清 registries 不 drop Library（reload 加 clear_tools/clear_commands）——
  引入 reload 机制变更 + 清空→重 populate 窗口的并发风险（host ToolSpec adapter 刷新需
  验证），scope 蔓延到 §F2b reload 侧。

### Q2 — source-spec 语法 = prefix
- **决策**：`git:<url>[@<ref>]` / `path:<dir>`；`crate:`/`prebuilt:` 识别但 stub。
  provenance 串 = 规范化后的 spec（`git:<url>@<ref>` / `path:<abs>`）。
- **理由**：匹配现有 stub precedent（测试传 `git:foo/bar`）、manifest `[source]{type,ref}`、
  state-file 示例 `git:github.com/foo/bar@v1`；parser 最简（首个 `:` 切分 prefix→impl）。
- **rejected**：flag 语法（`--git <url> --ref v`，偏离 stub precedent + 需 argparse）；
  URL 自动探测（隐式 + 歧义 + 无法表达 ref）。

### Q3 — CargoBuilder = shell out，接受 build.rs 执行
- **决策**：`cargo build --release --locked --message-format=json --target-dir <temp>`
  在 fetched 源目录跑；解析 stdout 的 `compiler-artifact` JSON 行，取 `target.kind` 含
  `"cdylib"` 的 `filenames[0]`；stderr 进 `ExtensionError::Install`；PATH 无 cargo →
  `Install("cargo not found on PATH; rust toolchain required")`。`build.rs` 执行 = 任意
  代码，按 §8.1 trust-the-source 接受（install 是用户主动发起 = trusted）。
- **理由**：ROADMAP 标 CargoBuilder must-have；§8.1 stance 一致。
- **impl 细节**：`--target-dir` 指向 per-build temp（不污染 fetched 源、不撞 workspace
  target-dir lock）；`--release`（installed = production）；`--locked`（用源 Cargo.lock，
  不可复现则 fail，supply-chain 卫生）。
- **rejected**：prebuilt-only（推后 CargoBuilder，与 ROADMAP must-have 冲突）。

### Q4 — 默认放置 scope = project
- **决策**：默认 project（`<workspace>/.codesmith/extensions/<id>/`），`--global` opt-in
  （`~/.codesmith/extensions/<id>/`）。
- **理由**：project install 被 §F5b trust gate 管控（untrusted 时 `apply_trust_gate` 丢，
  直到 FirstLoad/信任接受）→ 更安全 + workspace-scoped，且让 §F5b 的 FirstLoad/trust-gate
  机器在 install 路径真正被用上（global==true 恒留会绕过 gate，使 gate 闲置）。
- **rejected**：默认 global（与 global state file 对称 + 跨 workspace + 装即加载，但绕过
  trust gate）。

### Q5 — install 时的 trust = 装完即 warn
- **决策**：install trust-agnostic（fetch/build/place/provenance），读 `is_workspace_trusted`
  只为发消息；untrusted project install 返回**成功** + warn "won't load until workspace
  trusted (accept trust prompt or /trust, then /extension reload)"。
- **理由**：匹配 §F5b Model A（gate-at-discovery，非 install）；让 FirstLoad 流程走通：
  install(project,untrusted) → reload 时 gate 丢 → 用户接受信任(FirstLoad flip) → reload
  时 gate 留 → `load_dylib`。
- **rejected**：拒绝 untrusted project install（耦合 install 与 trust state + 打破
  trust-agnostic 对称 + 需 trust-first 顺序）。

### Q6 — 测试 = trait-DI + 两互补测
- **决策**：`Installer` 持 trait 对象 → 测试注入 `FakeBuilder`（返回 §F5b fixture dylib
  `CODESMITH_FIXTURE_DYLIB`）做 install→load e2e（练 fetch→place→写 manifest→discover→
  load→provenance round-trip，断言 `fixture_echo` bound）；真 `CargoBuilder` 在 tiny
  standalone throwaway crate（temp target-dir，无 lock 冲突、无 workspace 依赖树重建）
  上单测（练真 `cargo build --release --locked --message-format=json` + cdylib 路径解析）。
  无 tui e2e（§F5 precedent）。
- **理由**：§F5b T4 fixture e2e precedent；避开 §F5b 踩过的 cargo-子进程/target-dir lock
  坑（T4 明确回避了 cargo 子进程）。

### Q7 — nice-to-have = stub 两者
- **决策**：`CratesIoSource`/`PrebuiltDylibSource` stub（`UnimplementedSource`-style）；
  本切片只 impl `GitSource` + `LocalPathSource`。
- **理由**：ROADMAP 标 nice-to-have；CratesIo 需 registry HTTP+version+checksum，Prebuilt 需
  HTTP fetch+sha256 verify，各 ~1 task + 新 dep。切片聚焦。

### D8（新决策）— id/version 来源 = 临时加载 dylib 读 `metadata()`
- **决策**：install 的 fetch→build 后，用 §F5b `loader::load_dylib(cdylib)` **临时加载**
  刚构建的 dylib → `extension.metadata()` 取权威 `(id, version)` → 构造 `Placer{id, scope}`
  → place → 写 manifest(id/version/entry/source) → **drop 临时 Library + Box**（不
  `configure`、不 `register_tool`、不进 runner）。`entry` 字段省略 → discover 用
  `default_dylib_filename(id)` 解析（与 Placer 写的文件名一致）。
- **理由**：单一真相源（扩展代码本身），避免 Cargo.toml `[package] name/version` 解析脆
  性 + 不要求源仓库自带 `extension.toml`；与 §F5b loader 自然衔接。trust-the-source 一致
  （`codesmith_register_extension` 构造期可能跑代码 = §8.1）。
- **rejected**：从源 Cargo.toml 解析（脆 + crate name≠ext id 时失准）；要求源仓库带
  `extension.toml`（加重作者负担 + 双重真相源）；用户 `--id/--version` 传参（易错 + 非权威）。

## 6. Data flow

### install（`/extension install <spec> [--global]`）
1. `SourceSpec::parse(arg)` → `(kind, body, ref_opt, scope)`。`kind` ∈ {git,path,crate,
   prebuilt}；`crate`/`prebuilt` → 构造 `UnimplementedSource` → `fetch` 直接 `Err(Install)`。
2. 构造 `GitSource{url, ref}` 或 `LocalPathSource{dir}`；`dest` = fresh `tempdir()`。
3. `source.fetch(dest)` → `SourceArtifact{path: dest, provenance}`：
   - git：`git clone --depth 1 [--branch <ref>] <url> <dest>`（无 ref → 默认分支）；
     `git` 不在 PATH / clone 失败 → `Install`。provenance = `git:<url>` + (`@<ref>` 若有)。
   - path：`cp -r <dir> <dest>`（或 `fs::copy` 递归）；provenance = `path:<canonicalize(dir)>`。
4. `CargoBuilder.build(dest)` → cdylib `PathBuf`：
   `cargo build --release --locked --message-format=json --target-dir <temp2>`，stdout 按行
   解析 JSON，取 `reason=="compiler-artifact"` 且 `target.kind` 含 `"cdylib"` 的
   `filenames[0]`；无 cdylib target → `Install("no cdylib target in <crate>")`；cargo
   非 0 退出 → `Install(stderr 摘要)`。
5. **D8**：`loader::load_dylib(cdylib)` → `(Library, Box<dyn Extension>)` →
   `extension.metadata().{id, version}` → drop Box + Library。
6. `Placer{id, scope}.place(cdylib)` → `fs::create_dir_all(<root>/<id>)` + `fs::copy` 到
   `<root>/<id>/<default_dylib_filename(id)>`（复用 `default_dylib_filename`，改 `pub(crate)`）。
7. 写 `<root>/<id>/extension.toml`：
   ```toml
   id = "<id>"
   version = "<version>"
   [source]
   type = "<kind>"      # "git" | "path"
   ref = "<ref_opt>"    # 省略若无
   ```
   （`entry` 省略 → discover 用 default；`api_version` 省略。）
8. `state.add_installed(id, provenance)` → persist。
9. 读 `is_workspace_trusted(workspace)`：若 `scope==Project && !trusted` → `will_load=false`
   + warn。返回 `InstallReport{id, version, path, provenance, will_load}`。

### uninstall（`/extension uninstall <id>`）
1. 在 global root（`effective_home_dir()/.codesmith/extensions`）+ project root
   （`workspace/.codesmith/extensions`）找 `<id>/` 目录（convention-based 定位，不依赖
   state 存 scope）；找到则 `fs::remove_dir_all`（含 dylib + manifest）。
2. `state.remove_installed(id)` → persist。
3. 返回 warn：`"tools/commands remain bound until process restart (bounded retention,
   Q1); handlers clear on next /extension reload"`。**不** drop `Library`、不 clear_tools。

## 7. API reconciliation vs §F5b

| 项 | §F5b 现状 | §F5c 动作 |
|---|---|---|
| `discover_dylib` 签名 | 2-arg `(global, project)` | **不改**（plan 草稿 3-arg configured 是 drift；configured-paths out-of-scope） |
| `DiscoveredSource` | flat struct | **不改** |
| `apply_trust_gate` | `(sources, trust_untrusted)` | **不改** |
| `discovery.rs:181` 注释 | "§F5c refines this to keep project-*configured* sources even when untrusted" | **改**为 "§F5c keeps Model A as-is (no configured-path concept); `apply_trust_gate` unchanged"（configured-paths out-of-scope → refine 不发生） |
| `default_dylib_filename` | `discovery.rs:50` private fn | **改 `pub(crate)`**（Placer 复用，保文件名一致） |
| `ExtensionStateStore.installed` | `BTreeSet<String>` | **改 `BTreeMap<String,String>`**（id→provenance）；`OnDiskState.installed` 同改 → TOML `[installed]` table；slice 1 无真实数据→**无迁移** |
| `installed()` reader | `-> Vec<String>`（provenance 串） | **保留**（back-compat）；加 `installed_ids()->Vec<String>` / `provenance_for(id)->Option<String>` / `add_installed`/`remove_installed` mutator |
| `ManifestSource{kind,ref_}` | type+ref | **不改**（manifest [source] 只存 hint；完整 provenance 在 state file；`/extension info` 展示 provenance 从 state 查 id） |
| `loader::load_dylib`/`ExtensionRunner::load_dylib`/`load`/`metadata()` | §F5b | **复用不改**（D8 只临时调 `loader::load_dylib` 读 metadata） |
| `populate_extension_runtime`/`reload_extension_runtime` | §F5b | **不改**（install 不进 agent loop） |
| install-source traits | slice 1 trait 形状 | **不改 trait**，只加 impl（`GitSource`/`LocalPathSource`/`CargoBuilder`/`Placer`） |
| `UnimplementedSource` | slice 1 占位 | **复用**为 crate/prebuilt stub（带 kind 标记进 error msg） |

## 8. File / component map

- `crates/extensions/src/install_source.rs`（改）：加 `GitSource`/`LocalPathSource`/
  `CargoBuilder`/`Placer`(具体 impl) + `SourceSpec` + `InstallScope{Project,Global}` +
  复用 `UnimplementedSource`（crate/prebuilt 带 kind）。
- `crates/extensions/src/installer.rs`（**新**）：`Installer` orchestrator +
  `InstallReport`/`UninstallReport`。
- `crates/extensions/src/discovery.rs`（改）：`default_dylib_filename` → `pub(crate)`；
  修 `:181` stale 注释。
- `crates/extensions/src/lib.rs`（改）：re-export `Installer`/`SourceSpec`/`InstallScope`/
  `InstallReport`/`UninstallReport`/`GitSource`/`LocalPathSource`/`CargoBuilder`/`Placer`。
- `crates/tui/src/extension_state.rs`（改）：`installed`→`BTreeMap` + mutators + OnDiskState。
- `crates/tui/src/commands/extension_commands.rs`（改）：`install_stub`/`uninstall_stub`
  → 真实现（解析 `--global`、构造 `Installer`、调 install/uninstall、格式化 report/warn）。
- `crates/extensions/Cargo.toml`：**无新 crate dep**（GitSource/CargoBuilder 用
  `std::process::Command` 调 git/cargo；tempfile 已有；无 sha2 —— stub 了 prebuilt）。

## 9. Testing strategy

| 测试 | crate | 内容 |
|---|---|---|
| `SourceSpec::parse` | extensions | git/path/crate/prebuilt、ref 有无、`--global` flag 各路径 + 错误格式 |
| `ExtensionStateStore` mutators | tui | `add_installed`/`remove_installed`/`provenance_for`/`installed_ids` + persist round-trip + installed TOML table 形状 + 空文件默认 |
| `Placer` + manifest-write | extensions | place 到 temp root → 断言 `<id>/<default_dylib_filename(id)>` + `extension.toml` 存在 + manifest 字段 + `discover_dylib` re-find 为 manifest-subdir 源（非 bare） |
| `CargoBuilder` 单测 | extensions | synthesize tiny standalone cdylib crate 到 TempDir（`Cargo.toml`+`src/lib.rs` 含 `codesmith_register_extension`，无 workspace 依赖、temp `--target-dir`）→ `build()` → 断言返回 cdylib path + `loader::load_dylib` 能加载 |
| `GitSource` 错误路径 | extensions | `git` 不在 PATH / url 无效 → `ExtensionError::Install`（不依赖网络/真仓库；真 clone 留 `#[ignore]` or skip-on-no-git）+ provenance 格式断言 |
| **install→load e2e** | extensions | `Installer` 注入 `FakeSource`（dummy dest+provenance）+ `FakeBuilder`（返回 `CODESMITH_FIXTURE_DYLIB`）；**真 `Placer` + 真 manifest 写 + 真 state** → install 到 temp extensions root → `discover_dylib` 找到 → `ExtensionRunner::load_dylib` → `bind_core` → 断言 `fixture_echo` bound + `installed[]` 有 fixture id+provenance |

镜像 §F5b（T1-T3 单测 + T4 fixture e2e）。**无 tui e2e**。

## 10. Verification gate（切片末，不 commit）

- `cargo build --workspace` 全绿。
- `cargo test -p codesmith-extensions`：26 + §F5c 新增（SourceSpec/Placer/CargoBuilder/
  GitSource/e2e 各若干）→ 记真实计数。
- `cargo test -p codesmith-agent`：98（不变——install 不触 agent）。
- `cargo test -p codesmith-agent-runtime`：1163+2（不变——install 不触 host_executor；
  flaky `streamable_http_stale_session_reconnects_and_retries_tool_call` 隔离重跑绿）。
- `cargo test -p codesmith-tui --bin codesmith-tui`：2829 pass/26 pre-existing
  `runtime_api::tests` fail/2 ignored（§F5c 加 0 新失败；`extension_commands` 单测 +
  `extension_state` mutator 单测增量）。
- **grep（§F5c 新增）**：`GitSource`/`LocalPathSource`/`CargoBuilder`/`Placer`/`Installer`/
  `SourceSpec` in `crates/extensions/src` ≥1 各；`add_installed`/`remove_installed` in
  `crates/tui/src/extension_state.rs` ≥1；`installer.rs` 存在；`/extension install` 非
  stub（`install_stub` 删或调真 `Installer`）；`cargo build` in CargoBuilder ≥1；
  `compiler-artifact` JSON parse ≥1。
- **grep（§F5b 不变项）**：`libloading` in extensions `Cargo.toml` ≥1、
  `loader.rs`/`manifest.rs`/`build.rs` 存在、`discover_dylib` in `engine.rs` ≥1、
  `codesmith_register_extension` in fixture ≥1、`host_executor` `.emit`=16（不变）、
  `TrustReason::FirstLoad` in tui=1（不变）。

## 11. Honest-test red-line（沿用 §F5b）

- tui 26 个 `runtime_api::tests` PRE-EXISTING fail：环境性 HTTP-server 不 bind /
  connection-refused，无 panic；在 §F5b 前的 base `7a6819a7` 隔离重跑同样 fail——**非
  §F5c 回归**。报告写 "tui N pass/26 pre-existing runtime_api fail/2 ignored"，**不**说
  green、**不**算到 §F5c 头上。
- `agent-runtime` 的 `streamable_http_stale_session_reconnects_and_retries_tool_call`
  flaky（HTTP-server-bind），失败时隔离重跑绿。
- §F5c 不触 `host_executor.rs`（install 是 tui command 侧）→ `host_executor .emit`=16
  不变、`TrustReason::FirstLoad` in tui=1 不变。

## 12. References

- §F5b commits：`6891d605`(T1 manifest) → `6281ad26`(T2 loader) → `8c34adf4`(T3 discovery)
  → `5b6238bd`(T4 fixture) → `a3224825`(T5 wiring) → `770f2aff`(T6 list/info) →
  `93240b74`(T7 docs)。
- §F5b spec：`docs/superpowers/specs/2026-07-22-codesmith-extension-system-slice-5b-design.md`
- §F5b plan：`docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-5b.md`
  （**仅结构参考，其 API 描述有 drift——以本文 §2 代码 shape 为准**）
- ROADMAP §F5b 进度块 `:2575-2604`；EXTENSIONS intro `:1-39` + Sandbox Stance `:268-286`。
- 工具链：普通 `cargo`（rustc 1.90.0 / edition 2024 默认）。
