# TODO — CodeSmith §F extension system (handoff)

> Handoff note. Current as of the §F5 post-closure housekeeping
> (EXTENSIONS.md:91 deferred→delivered + §F umbrella/§F5b/§F5c specs
> tracked + this todo.md refresh; branched from `main` at `8b37d6fe`
> and ff-merged). **§F5 (the dylib-loading extension-system phase) is
> fully complete.** §F2 (events / handler chains / per-variant
> subscription / catch_unwind / live reload) delivered via §F2a/b/c.
> Next phases are **on-demand** (no spec/plan yet) — start any with
> the brainstorming skill → spec → plan → subagent-driven-development
> (same flow as §F5b-e).

## Current state

- **Last slices done**: §F5e (CratesIo + Prebuilt `INSTALL` source
  impls, closing the §F5c-deferred work) + 2 rounds of §F5e
  final-review housekeeping (UnimplementedSource dead-code removal +
  unused-import fix; then this post-closure round).
- **§F5 delivered**: `libloading` loader + `~/.codesmith/extensions/`
  discovery + `extension.toml` manifest + project-local trust prompt
  (FirstLoad) + real `/extension install`/`uninstall`/`reload` +
  install-source abstractions (Git / LocalPath / CratesIo /
  PrebuiltDylib) + two-phase `Library` drop safe at the turn boundary
  + host ext tools/commands wired live per-turn.
- **Working tree**: clean except `.zcode/` (local tooling) +
  `docs/superpowers/plans/` (untracked working artifacts per §F5d
  convention).
- **Build/test baseline** (all green, **plain cargo** — no toolchain
  override; the slice-1-era toolchain pin is dropped):
  - `cargo build --workspace` (142 tui warnings = pre-existing
    baseline, not new)
  - `cargo test -p codesmith-extensions --lib`: 75 pass + 1 ignored
  - `cargo test -p codesmith-agent --lib`: 98 pass
  - `cargo test -p codesmith-agent-runtime --lib`: 1165 pass + 2
    ignored (streamable_http flaky — pre-existing; isolate-rerun if
    fires)
  - `cargo test -p codesmith-tui --bin codesmith-tui`: 2867 pass + 2
    ignored (runtime_api flaky — pre-existing)

## Next options (on-demand — needs brainstorming→spec→plan before code)

- **§F3**: EventBus real impl (currently `crates/extensions/src/bus.rs`
  `subscribe`/`publish` return
  `ExtensionError::Unimplemented("EventBus.subscribe (§F3)")` /
  `"EventBus.publish (§F3)"` at ~lines 32 + 41).
- **§F4**: registerProvider.
- **§F6**: Renderers. **§F7**: Shortcut + Flag. **§F8**: Embedding API.
- (Hot-load permanently out — spec §2.4 "never".)

## Pointers

- §F umbrella design: `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md`
- §F5 slice specs (5b/5c/5d/5e): `docs/superpowers/specs/2026-07-2[2-7]-codesmith-extension-system-slice-5[b-e]-design.md`
- Recent plans (untracked, working artifacts): `docs/superpowers/plans/`
- ROADMAP §F section: `ROADMAP.md` (§F5 progress block at ~line 2676).
