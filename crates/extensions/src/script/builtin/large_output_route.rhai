// Builtin default policy: large-output routing thresholds. The routing
// mechanism (token estimation, workshop synthesis via the utility LLM,
// prompt defusing) stays in the host
// (`crates/agent-runtime/src/tools/large_output_router.rs`); this policy
// owns the numbers. Explicit `[workshop]` values in config.toml
// (`large_output_threshold_tokens` / `per_tool_thresholds`) sit above
// this tier: config wins, this supplies the default when TOML is silent.
//
//   threshold_tokens — estimated-token count above which a tool result
//                      is routed through the workshop synthesis
//   per_tool         — tool name → threshold overrides

register_policy("large-output-route", #{
    threshold_tokens: 4096,
    per_tool: #{},
});
