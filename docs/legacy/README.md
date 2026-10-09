# Legacy archives

This directory preserves pre-fork historical material from the upstream
projects (deepseek-tui → CodeWhale) that CodeSmith is derived from:

- [`CHANGELOG-upstream.md`](CHANGELOG-upstream.md) — the complete upstream
  release history through v0.8.48, kept verbatim. Links inside point at the
  upstream repository and its issue numbers.
- [`ROADMAP-upstream.md`](ROADMAP-upstream.md) — the upstream internal
  development roadmap (slice-level notes). Referenced by provenance comments
  in the source code (e.g. `ROADMAP §E`).
- [`REVIEW_PIPELINE.md`](REVIEW_PIPELINE.md) /
  [`REVIEW_PIPELINE_cn.md`](REVIEW_PIPELINE_cn.md) — the upstream
  community-PR review pipeline (review bots, `autonomous-ready` merge
  queue, post-merge automation). The workflows it describes were removed;
  contribution landing mechanics now live in
  [../../CONTRIBUTING.md](../../CONTRIBUTING.md).
- [`RLM_BRANCHING_ROADMAP.md`](RLM_BRANCHING_ROADMAP.md) /
  [`RLM_BRANCHING_ROADMAP_cn.md`](RLM_BRANCHING_ROADMAP_cn.md) — the
  upstream RLM-branching roadmap. Its v0.8.45–v0.8.48 items landed before
  the fork; the v0.9/v0.10 items were never built, and the milestone
  numbering refers to upstream releases, not CodeSmith versions.

These files are frozen history: they are not maintained, and paths, issue
numbers, and environment names in them may no longer exist in CodeSmith.
See [../../README.md](../../README.md) for the project lineage and contributor
attribution.
