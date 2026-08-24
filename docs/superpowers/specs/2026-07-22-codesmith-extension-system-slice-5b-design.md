# §F5b — Dylib LOAD 侧(phase 2 续作·上)设计规格

- **日期:** 2026-07-22
- **状态:** 设计(brainstorm 已完成,Q1–Q4 已拍板;等待 user review 后 `writing-plans` 出实施计划)
- **范围:** §F5 的 **LOAD 半** —— dylib loader + `extension.toml` manifest + phase-2 三源发现 + 项目本地 trust gate(consume `FirstLoad`)+ reload wiring。**INSTALL 半 = §F5c,defer。**
- **分支:** `feat/pluggable-framework-core`
- **前置:** §F5 slice 1(`ProjectTrust{FirstLoad}` trust-prompt emit site,commit `283aec12`——`FirstLoad` 事件已 emit 但无 dylib 机器 consume 它)。本切片是该 emit 的第一个真实 consumer。
- **参考:**
  - `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md`(§F 整体 spec):§2.4 永不 hot-load / §6.1 reload 序列 / §6.4 install-source 抽象 / §7.2 phase-2 发现 + manifest + 项目本地 trust gate / §8.2 lockstep / §9 §F5=全量 dylib / §10 slice 范围与测试 shape / §11 open questions。
  - `docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-5.md`(§F5 slice 1 plan):Design decisions / Architecture / Baseline / File Structure / Task / Verification gate / Out of scope 格式样板。
  - `docs/EXTENSIONS.md`(host-seam 表 + Sandbox Stance)。

---

## 1. 范围(slice 5b = LOAD 侧;镜像整体 spec §10.1)

本切片落地 §F5 全量 dylib 机器的 **LOAD 半**:把磁盘上的 dylib 文件 + `extension.toml` manifest 发现、解析、载入 `ExtensionRunner`,经项目本地 trust gate 守门,reload 自动拾取。install/uninstall(fetch/compile/place/provenance 写)保持 stub → §F5c。

**落地内容:**

- **`crates/extensions/src/manifest.rs`(NEW):** `ExtensionManifest`(serde Deserialize)—— `id` / `version` / `entry`(可选,默认 `<id>.<dylib-ext>`) / `source`(可选 provenance) / `api_version`(可选,Q4)。`parse(path)` / `from_str` + round-trip 测试。
- **`crates/extensions/src/loader.rs`(NEW):** `load_dylib(path) -> Result<(Library, Box<dyn Extension>), ExtensionError>`——`libloading::Library::new` → 取 `codesmith_register_extension` symbol → `Box::from_raw` → 返回 `(library, extension)`。所有 `unsafe` 集中于此 + 文档化 lockstep 前提(§8.2)。`ExtensionRunner` 加 `libraries: Mutex<Vec<Library>>` 字段 + `async fn load_dylib(&self, path)`(push library → `self.load(&*ext)`,ext Box 在 configure 后 drop,library 留存)。Q1。
- **`crates/extensions/src/discovery.rs`(扩展):** `discover_dylib(workspace, configured) -> Vec<DiscoveredDylib>`(三源:全局 `~/.codesmith/extensions/`、项目 `.codesmith/extensions/`、配置路径;一层深;`*.dylib`/`*.so`/`*.dll` 裸文件 或 带 `extension.toml` 的子目录;按 resolved path 去重 first-wins)。`DiscoveredSource { Global, ProjectLocal, ConfiguredPath }` tag。`apply_trust_gate(entries, project_trusted: bool) -> Vec<DiscoveredDylib>`(纯函数:`!project_trusted` 时丢 `ProjectLocal`——trust-gate 测试落于此,Q2 Model A)。
- **`crates/extensions/Cargo.toml`:** 加 `libloading` + `toml`;确保 `serde` 的 `derive` feature(manifest `#[derive(Deserialize)]`)。
- **`crates/tui/src/core/engine.rs`(扩展):** `populate_extension_runtime`(`:378`)在 `discover_static()`(`:385`)之后加 `discover_dylib` 步骤:`apply_trust_gate(discover_dylib(...), crate::config::is_workspace_trusted(workspace))` → 与 `state` reconcile(skip disabled)→ 对每个 dylib 在 load-rt 上 `runner.load_dylib(path)`(镜像静态的 OS-thread single-thread runtime 模式 `:404-418`)。`reload_extension_runtime`(`:447`)不变——它调 `populate`,故 reload 自动拾取 dylib(Q1:reload 不清 `libraries`)。
- **`crates/tui/src/commands/extension_commands.rs`(扩展):** `list`(`:56`)/ `info`(`:69`)在 `discover_static()` 之外也枚举 `discover_dylib(...)`(去重后),使 dylib-discovered ext 可见。`install_stub`(`:150`)/`uninstall_stub`(`:156`)保持 stub → §F5c。
- **一个 workspace-member cdylib fixture crate**(Q3):`crate-type = ["cdylib"]`,导出 `#[no_mangle] pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension`,返回注册了一个 tool + 一个 handler 的 `Box<dyn Extension>`(镜像 `sample_scratchpad.rs` 的 `ScratchTool` + `TurnStartLogger`)。fixture 依赖 `codesmith-agent` + `codesmith-tools`(同 workspace、同 toolchain → lockstep 由构造保证)。测试在 `codesmith-extensions` 经其 `build.rs` 发的 `CODESMITH_FIXTURE_DYLIB` env 载入 fixture artifact,断言 tool 进 `bound_tools()`、handler 在 `emit` 后触发(`build.rs` 解析 target dir + 确保已构建,见 §7 T4)。
- **`docs/EXTENSIONS.md` + `ROADMAP.md`:** host-seam dylib 行 + Sandbox Stance(phase-2 dylib loader landed;trust gate Model A)+ §F5b 进度块 + `### F5b` 子节(LOAD 半 done,INSTALL 半 → §F5c)。

## 2. 显式 defer(镜像 §10.2;§F5c = INSTALL 侧)

- **install-source impls**(`GitSource` / `LocalPathSource` must-have;`CratesIoSource` / `PrebuiltDylibSource` nice-to-have,整体 spec §11 hint,§F5c 终定)→ §F5c。`install_source.rs` 的 trait(`ExtensionSource`/`Builder`/`Placer`,`:20-32`)与 `UnimplementedSource` stub(`:37-44`)本切片不动。
- **`CargoBuilder`(cargo build --release)+ `Placer`(→ `~/.codesmith/extensions/<id>/`)** → §F5c。
- **`/extension install` / `uninstall` 真实现** → §F5c(保持 stub `install_stub`/`uninstall_stub`)。
- **`installed[]` provenance 写**(present `ExtensionStateStore.installed` getter `:106`,无 mutator)→ §F5c。§F5b 只读用于 `list`/`info` 显示。
- **`abi_stable`**(重依赖 + macro 驱动 trait 形状 churn 稳定契约)—— **rejected**,违 §2.4「dylib loader 后续包同一个 trait——无 ABI churn」。本切片用 raw `libloading` + lockstep(§8.2)。
- **hot-load** —— 永不(§2.4)。reload 是 clean break。
- **完整事件集 emit wiring**(§F2 / §F3+)、`EventBus` impl、`registerProvider`、renderer/shortcut/flag —— 不变,各属其 §F slice。

---

## 3. 架构

镜像 §E 三层 + 整体 spec §3 分层规则:`codesmith-extensions`(loader/discovery/manifest/runner)依赖 `codesmith-agent`(traits),从不反向;`codesmith-tui`(host wiring)依赖 `codesmith-extensions`。trust 状态定义在 `codesmith-agent-runtime/src/workspace_trust.rs:116`(`is_workspace_trusted`),经 `crates/tui/src/config.rs:2640` `pub(crate) use` re-export 暴露给 tui——**discovery 保持 trust-agnostic**(只返回 tagged entries),trust gate 由 host 层(tui `populate`)以 bool 注入 `apply_trust_gate`,故 trust-gate 逻辑可在 `codesmith-extensions` 单测(无需 tui/agent-runtime 依赖)。

```
discover_dylib(workspace, configured)         [codesmith-extensions — pure, no trust-state dep]
  └─▶ Vec<DiscoveredDylib{ manifest, path, source: Global|ProjectLocal|ConfiguredPath }>
         │
         ▼  (host layer, tui engine.rs populate_extension_runtime)
apply_trust_gate(entries, is_workspace_trusted(workspace))   [drops ProjectLocal when !trusted]
  └─▶ reconcile w/ ExtensionStateStore (skip disabled)
         │
         ▼  (OS-thread single-thread runtime, mirrors static load :404-418)
for each dylib: runner.load_dylib(path)
  └─▶ loader.load_dylib(path) ── Library::new ──▶ Symbol "codesmith_register_extension"
         │                         └─▶ *mut dyn Extension ── Box::from_raw ──▶ Box<dyn Extension>
         │  push Library into runner.libraries   (Q1: outlives registries)
         └─▶ runner.load(&*ext).await           (configure → Pending; ext Box drops)
         │
         ▼  bind_core (existing :167)
drain Pending → tools/commands/handlers registries (Arc<dyn ToolDefinition/...>; vtable+code in kept Library)
```

**两阶段构造不变:** `load`(`runner.rs:155`)取 `&dyn Extension` 跑 `configure`,**不保留 Extension**——只 `Pending` 的 Arc 贡献存活(已验证:`sample_scratchpad.rs` 的 `ScratchTool`/`TurnStartLogger` 是 self-contained owned 对象,不捕获 Extension;读 `static SCRATCH` 而非 `&self`)。dylib 路径镜像此:configure 后 ext Box drop,注册的 trait 对象 own 自己的数据 + 引用 Library 的 vtable/code——**故 Library 必须 outlive runner registries**。

**Q1 的 unsafe 正确性(lockstep 对齐 §8.2):** `codesmith_register_extension` 在 dylib 内 `Box::new(MyExt)` + `Box::into_raw` 返回 `*mut dyn Extension`;host `Box::from_raw` 取回所有权后 drop。allocator 一致性由 lockstep 保证(同 compiler + 同 `codesmith-agent` 版本 → 同全局 allocator)。`*mut dyn Extension` 是 fat pointer(data + vtable,2 word);经 `extern "C"` 返回的 fat-pointer 表示在 same-compiler builds 间一致(lockstep 兜底)——这是 raw libloading(非 `abi_stable`)的既定权衡,整体 spec §2.4/§8.2 已锁定。

**Library 生命周期(Q1)与 registry-clear 不对称:** `bind_core`(`runner.rs:167-189`)对 `tools`/`commands` 是 **append-insert**(HashMap 同 key 覆盖→旧 Arc drop;不同 key 累积),`handlers` 是 Vec append;`reload` 只调 `clear_handlers`(`:145`,清 handlers)——**无 `clear_tools`/`clear_commands`**(已验证)。后果:
- reload 重发现**同一 dylib** + 重注册同 key tool → 旧 Arc 被覆盖 drop → 旧 Library 的 code 不再被引用,但 Library 仍留存 = **有界泄漏**(extensions×reloads,罕见)。
- reload **移除某 dylib** → 其旧 tool Arc 因无 `clear_tools` 仍存活于 HashMap → 仍引用旧 Library vtable → **此时留存 Library 是正确性必需**(否则悬垂 vtable = UB)。
- 故 Q1「reload 不清 `libraries`」不是单纯「可接受泄漏」——对 registry-clear 不对称的现状,它是**正确性保底**:tools/commands 不清则旧 Library 不能释。代价 = bounded memory,可接受;真正干净卸载需 `clear_tools`/`clear_commands` + `Library` 析构协调,超出本切片(留 §F5c/后续)。

---

## 4. 设计决策(load-bearing —— brainstorm 已定稿,Q1–Q4 已 user-confirmed;勿重探 intent/requirements/design)

### 范围 fork(已定):split LOAD/INSTALL
本切片 = **LOAD 侧**(loader + manifest + 发现 + trust gate + reload wiring);install/uninstall(fetch/compile/place/provenance 写)保持 stub → **§F5c**(install-source impls[git + local path must-have;crates.io + prebuilt nice-to-have,整体 spec §11 hint,§F5c 终定] + `CargoBuilder` + `Placer` + `/extension install`/`uninstall` 真实现 + `installed[]` provenance 写)。

### ABI fork(已定):raw `libloading` + lockstep `*mut dyn Extension`(Approach 1)
dylib 导出 `#[no_mangle] pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension`;loader `Box::from_raw` 后喂 `runner.load(&*ext).await`——与编译进来 extension **同一路径**,trait 派发全 Rust-ABI in-process。lockstep(整体 spec §8.2:同 compiler + 同 `codesmith-agent` 版本 → vtable 匹配)。**无新 trait 形状**——逐字兑现 §2.4「dylib loader 后续包同一个 trait——无 ABI churn」。无 `abi_stable`(重依赖 + macro 驱动 trait 形状 churn 稳定契约,违 §2.4)。`ExtensionError` 已有 `Load(String)`(`crates/agent/src/extension.rs:65`)/ `Install(String)`(`:63`)变体——dylib-load 错误面已在契约内,无需扩 enum。

### Q1 Library 生命周期(confirmed:Runner 持有 Vec)
`ExtensionRunner` 加 `libraries: Mutex<Vec<Library>>` 字段,`load_dylib` push 进去,**reload 时不清**。理由见 §3「Library 生命周期」:对 registry-clear 不对称(只 `clear_handlers`)的现状,留存是正确性保底(移除的 dylib 的 tool Arc 仍存活);重发现同 dylib 则成有界泄漏(extensions×reloads,罕见可接受)。备选(host 持有 Vec / track-by-path-replace)rejected——前者增 host↔runner 协调面,后者需 path→Library 索引 + 卸载时序(v1 复杂度偏高)。Manual `Debug` impl(`runner.rs:298-320`)扩展加 `libraries` 计数,保持 `EngineHost` 的 `#[derive(Debug)]`。

### Q2 trust gate / consume FirstLoad(confirmed:Model A)
discover 时以 `crate::config::is_workspace_trusted(workspace)`(re-export 自 `codesmith-agent-runtime/src/workspace_trust.rs:116`,被 `onboarding/mod.rs:156` 引用;`save_workspace_trust` 在 `config.rs:2643`)为门:**不信任则跳过项目本地 dylib**。onboarding 接受 → `mark_trusted`(`onboarding/mod.rs:167`)→ `save_workspace_trust` → `is_workspace_trusted` 翻 true(即 `FirstLoad` emit 翻转时刻 `tui/ui.rs:2879-2896`)。随后 `/extension reload` 或重启(信任已持久化)拾取项目本地 dylib。**不给 onboarding 路径加 reload**——信任翻转后由用户显式 reload 或重启拾取。`FirstLoad` 事件(§F5 slice 1 emit)与 trust gate 的关系:gate 读的是**持久化信任状态**(`is_workspace_trusted`,`FirstLoad` 接受翻转它),非事件本身——故 gate 是 `FirstLoad` 语义的第一个真实 consumer,但**不直接监听事件**(避免 reload-on-accept 的时序复杂度,见备选 Model B rejected)。备选 Model B(接受臂自动 reload)rejected——给 onboarding 路径加一次 reload + reload 时序需信任已持久化后才安全。
- **分层:** discovery 返回 tagged entries(trust-agnostic);`apply_trust_gate(entries, project_trusted: bool)` 是纯函数,host `populate` 传 `is_workspace_trusted(workspace)` 的 bool。trust-gate 单测在 `codesmith-extensions`(`apply_trust_gate` + mock bool),无需 tui fixture。
- **命名辨析:** `crates/tui/src/workspace_trust.rs`(外部路径 allowlist,`~/.codesmith/workspace-trust.json`,非 persisted workspace trust)——**勿混**;本切片 trust gate 用 `config::is_workspace_trusted`(persisted `[projects]` trust),已验证。

### Q3 测试 fixture(confirmed:独立 cdylib fixture crate)
workspace 内一个 `crate-type=["cdylib"]` fixture crate(`crates/extensions-fixture-dylib`),导出 `codesmith_register_extension` 返回注册了 tool + handler 的 `Box<dyn Extension>`。测试经 `CARGO_TARGET_DIR`(或 build.rs env,plan 终定)载 `target/<profile>/lib<name>.<dylib_ext>`(`std::env::consts::{DLL_PREFIX, DLL_EXTENSION}`),断言 tool 进 `bound_tools()`、handler 在 `emit` 后触发。+ manifest parse 测试 + 发现测试(tempdir 伪 dylib + `extension.toml` + path 去重 + source tag)+ trust-gate 测试(`apply_trust_gate` with `project_trusted=false` 丢 `ProjectLocal`)。
- **target-dir 定位细节**(profile/debug-vs-release、`build.target-dir` config、fixture build 顺序)在 `writing-plans` 阶段 finalize(可能 build.rs 发 `cargo:rustc-env=EXTENSIONS_FIXTURE_DYLIB_PATH=<abs>` 或测试内 `cargo build -p extensions-fixture-dylib` 前置);spec 只承诺「workspace-member fixture + 测试从 target dir 定位 artifact」策略 + 测试 shape。
- **lockstep 由构造保证:** fixture 同 workspace、同 `1.90.0` toolchain、同 `codesmith-agent` 版本 → `Box::from_raw` + fat-pointer return 跨 ABI 一致。
- 备选(build.rs 内联生成 / 提交预编译 dylib)rejected——前者跨平台/CI 更脆,后者平台脆弱 + 违 lockstep 精神。

### Q4 manifest api_version(confirmed:可选字段,warn 不 refuse)
manifest 含可选 `api_version` 字段;loader 在 present + 不匹配时 `tracing::warn`(不 refuse——lockstep 由 build 强制,非 runtime 校验职责,对齐整体 spec §8.2「manifest 无 per-extension 声明的 API 版本(隐式由 dep 版本)」;本切片在此基础上加**可选可见性**,不把它升级为硬门)。备选(必填 refuse / 完全不设)rejected——前者把 build-time lockstep 责任下放 runtime(违 §8.2),后者失去 manifest 声明期望版本的任何可见性。

---

## 5. Baseline(切片末不得回退;post-§F5 slice-1,commit `283aec12`)

`codesmith-extensions --lib` 15 · `codesmith-agent --lib` 98 · `codesmith-agent-runtime --lib` 1163+2 · `codesmith-tui --bin codesmith-tui` 2855+2 · `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16 · `grep -rn 'TrustReason::FirstLoad' crates/tui/src` = 1(`tui/ui.rs:2892`)。

**§F5b 前置状态 grep(本切片从 0 增长):** `grep -c 'libloading' crates/extensions/Cargo.toml` = 0 · `grep -n 'toml' crates/extensions/Cargo.toml`(dep)= 0(注:`serde` 已在)· `loader.rs`/`manifest.rs` 不存在 · `grep -rn 'discover_dylib' crates/` = 0 · `grep -rn 'codesmith_register_extension' crates/` = 0。

> **Pre-existing flaky test(非回退——勿修):** `mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call`(`crates/agent-runtime/src/mcp.rs:5489`)并行负载下偶发失败(mock-server race),隔离重跑绿。§F5b dylib 不碰 `mcp.rs`。切片末若 `agent-runtime` 仅此 1 失败,隔离重跑确认绿后视门达成;期望绿态 = 1163 passed + 2 ignored。

---

## 6. 文件结构(modified / added)

**`crates/extensions/`(modified)**
- `Cargo.toml` —— T1:加 `libloading` + `toml` dep;`serde` `derive` feature(manifest `#[derive(Deserialize)]`)。
- `src/lib.rs` —— T1/T2/T3:`pub mod manifest;` `pub mod loader;` + re-export(`discover_dylib` / `load_dylib` / `ExtensionManifest` / `DiscoveredDylib` / `DiscoveredSource`)。
- `src/manifest.rs`(NEW)—— T1:`ExtensionManifest` + `parse` / `from_str` + 测试。
- `src/loader.rs`(NEW)—— T2:`load_dylib(path)` + `ExtensionRunner::libraries` 字段 + `load_dylib` 方法 + 测试(symbol 缺失/null/error 路径)。
- `src/discovery.rs`(modified)—— T3:`discover_dylib` + `DiscoveredDylib`/`DiscoveredSource` + `apply_trust_gate` + 测试(发现/去重/tag/trust-gate)。
- `src/runner.rs`(modified)—— T2:`libraries: Mutex<Vec<Library>>` 字段(`:87-103` struct)+ `load_dylib` async 方法 + Manual `Debug`(`:298-320`)加 `libraries` 计数。
- `build.rs`(NEW)—— T4:确保 fixture cdylib 已构建并经 `cargo:rustc-env=CODESMITH_FIXTURE_DYLIB=<path>` 发路径给测试(见 §7 T4 build-ordering)。

**`crates/extensions-fixture-dylib/`(NEW workspace member;root `Cargo.toml` `[workspace].members` 加 `"crates/extensions-fixture-dylib"`)**
- `Cargo.toml` —— T4:`crate-type = ["cdylib"]`,dep `codesmith-agent` + `codesmith-tools`(+ `async-trait`/`serde_json` 按 sample 需)。
- `src/lib.rs` —— T4:`#[no_mangle] pub extern "C" fn codesmith_register_extension() -> *mut dyn Extension` + 注册 tool + handler 的 `Box<dyn Extension>`(镜像 `sample_scratchpad.rs`)。

**`crates/tui/src/core/engine.rs`(modified)** —— T5:`populate_extension_runtime`(`:378-434`)在静态 discover/reconcile/load 之后、`bind_core`(`:433`)之前加 dylib discover→trust-gate→reconcile→`load_dylib` 段(同一 OS-thread runtime)。`reload_extension_runtime`(`:447`)不变。

**`crates/tui/src/commands/extension_commands.rs`(modified)** —— T6:`list`(`:56`)/`info`(`:69`)加 `discover_dylib(...)` 枚举(去重后显示)。`install_stub`/`uninstall_stub` 不变。

**docs** —— T7:`docs/EXTENSIONS.md`(host-seam dylib 行 + Sandbox Stance:phase-2 dylib LOAD landed,trust gate Model A;INSTALL → §F5c)+ `ROADMAP.md`(§F5 进度块 + `### F5b` 子节 + §F2c/§F5 next-focus 更新)。

---

## 7. 任务分解(T1–T7 prose;`writing-plans` skill 展开为 checkbox Red→impl→Green→commit)

> 每个 task 单独 commit,message 风格 `feat(framework): §F5b Tn ...`。TDD 顺序依依赖:T1 manifest → T2 loader → T3 discovery(需 manifest)→ T4 fixture(需 loader)→ T5 wiring(需 loader+discovery)→ T6 list/info(需 discovery)→ T7 docs。

- **T1 — manifest + Cargo deps + parse 测试。** `manifest.rs`(`ExtensionManifest` serde Deserialize:`id`/`version`/`entry: Option`/`source: Option`/`api_version: Option`) + `Cargo.toml`(libloading + toml + serde derive)+ `lib.rs` re-export。Red:parse 测试失败(无模块);impl;Green。测试 shape:parse 一个样例 `extension.toml`(含/不含 `entry`/`api_version`)→ 断言字段 + 默认 `entry` 推导。
- **T2 — loader + runner.libraries/load_dylib + 错误路径测试。** `loader.rs`(`load_dylib(path) -> Result<(Library, Box<dyn Extension>)`,unsafe 集中 + lockstep 注释)+ `runner.rs`(`libraries` 字段 + `async fn load_dylib(&self, path)` push library 后 `self.load(&*ext)`)+ `Debug` 扩展。Red:loader 错误测试(不存在文件 → `ExtensionError::Load`;缺 symbol → `Load`;null ptr → `Load`)失败;impl;Green。注:真 load-contributions 测试在 T4(fixture);T2 只覆盖错误路径(不依赖真 dylib)。
- **T3 — discover_dylib + apply_trust_gate + 发现/trust-gate 测试。** `discovery.rs`(`discover_dylib(workspace, configured) -> Vec<DiscoveredDylib>` 三源 + 一层深 + path 去重 first-wins + `DiscoveredSource` tag;`apply_trust_gate(entries, project_trusted: bool)`)。Red:tempdir 伪 dylib + `extension.toml` + 重复 path → 断言发现/去重/tag;`apply_trust_gate` with `false` → `ProjectLocal` 被丢;impl;Green。
- **T4 — fixture crate + load-contributions 集成测试。** `crates/extensions-fixture-dylib`(cdylib,`codesmith_register_extension` 返回注册 tool+handler 的 Box<dyn Extension>)+ `codesmith-extensions` `build.rs`(构建 fixture + 发 `cargo:rustc-env=CODESMITH_FIXTURE_DYLIB=<path>`)+ 测试:载 emitted path → `runner.load_dylib` → `bind_core` → 断言 tool in `bound_tools()`、`emit` 触发 handler。
  - **build-ordering 约束(plan 终定机制):** fixture 非 `codesmith-extensions` 的 dep,故 `cargo test -p codesmith-extensions --lib`(验证门 cmd)默认**不构建**它——该 cmd **必须自足**,由 `build.rs` 在 codesmith-extensions 编译时(含 test)确保 fixture 已构建 + 发路径。机制二选一(plan 据实定):(a) `build.rs` shell `cargo build -p extensions-fixture-dylib` 到**独立 target dir**(避开父 cargo 的 target-dir lock 死锁)→ 发路径;(b) `build.rs` 直接 `rustc --crate-type cdylib` 编 fixture 源(避 cargo,但需手摆 `--extern` dep rlib 路径)。lockstep 由构造保证(同 workspace + 同 1.90.0 toolchain)。
- **T5 — engine.rs wiring(populate dylib 段)+ reload 验证。** `populate_extension_runtime` 加 dylib discover→trust-gate→reconcile→`load_dylib` 段(同一 OS-thread runtime)。Red/验证:`/extension reload` 在 fixture-on-disk 场景重载(若 tui e2e fixture 比例失衡,defer per §F5 slice 1 precedent,以 runner-level + wiring 单测代之)。`reload_extension_runtime` 不变(reload 自动拾取 dylib)。
- **T6 — list/info surface dylib ext + 测试。** `extension_commands.rs` `list`/`info` 加 `discover_dylib(...)` 枚举(去重后)。Red:`/extension list` smoke 显示 dylib ext;impl;Green。`configured` 路径读取(`settings.extensions[]`)plan 终定。
- **T7 — docs(EXTENSIONS + ROADMAP §F5b)。** host-seam dylib 行 + Sandbox Stance(phase-2 dylib LOAD landed;trust gate Model A;INSTALL → §F5c)+ §F5 进度块 + `### F5b` 子节(LOAD 半 done,INSTALL → §F5c)+ next-focus 更新。docs-only,绿态不变。

## 8. 验证 gate(切片末,`cargo +1.90.0`;非 commit)

- [ ] `cargo +1.90.0 build --workspace` 绿(含新 fixture cdylib crate)。
- [ ] `cargo +1.90.0 test -p codesmith-extensions --lib` = 15 + N(manifest parse / loader 错误 / discover_dylib 去重+tag / `apply_trust_gate` / fixture load-contributions)。**build.rs 自足构建 fixture**,无需前置 `cargo build`。
- [ ] `cargo +1.90.0 test -p codesmith-agent --lib` = 98(不变——契约 read-only,无 enum/trait 变更)。
- [ ] `cargo +1.90.0 test -p codesmith-agent-runtime --lib` = 1163+2(不变——LOAD 不触 `host_executor`;若仅 `streamable_http_stale_session...` 失败,隔离重跑确认绿)。
- [ ] `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` = 2855+2 + wiring/list 测试(若 T5/T6 加 tui 测试)。
- [ ] `grep -c 'libloading' crates/extensions/Cargo.toml` ≥ 1;`grep -n 'toml' crates/extensions/Cargo.toml` dep 存在。
- [ ] `ls crates/extensions/src/{loader,manifest}.rs` 存在。
- [ ] `grep -rn 'discover_dylib' crates/tui/src/core/engine.rs` ≥ 1(populate 调用点)。
- [ ] `grep -rn 'codesmith_register_extension' crates/extensions-fixture-dylib/` ≥ 1(fixture 符号)。
- [ ] **must-not-regress:** `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16;`grep -rn 'TrustReason::FirstLoad' crates/tui/src` = 1。

## 9. Out of scope(显式 defer,见 §2 + T7 doc)

- **§F5c INSTALL 侧:** install-source impls(Git / LocalPath must-have;CratesIo / Prebuilt nice-to-have)+ `CargoBuilder` + `Placer` + `/extension install`/`uninstall` 真实现 + `installed[]` provenance 写(`ExtensionStateStore` 无 mutator,§F5c 加)。本切片 `install_stub`/`uninstall_stub` 保持 stub。
- **`abi_stable`** —— rejected(§2.4 无 ABI churn;raw libloading + lockstep)。
- **`clear_tools`/`clear_commands` + Library 真卸载** —— 留 §F5c/后续(本切片 Q1 接受 bounded 留存保底正确性)。
- **hot-load** —— 永不(§2.4;reload 是 clean break)。
- **完整事件集 emit wiring**(§F2/§F3+)、`EventBus` impl、`registerProvider`、renderer/shortcut/flag —— 各属其 §F slice,不变。
- **tui-level e2e(run_tui 触发 dylib 发现/reload)** —— 若 fixture 比例失衡,defer per §F5 slice 1 / §F2b `SessionBeforeSwitch` precedent;以 runner-level + wiring 单测代之。
