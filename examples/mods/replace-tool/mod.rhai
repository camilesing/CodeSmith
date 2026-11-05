// Replace-tool (DSH pattern d) as a runtime-swappable Rhai mod.
//
// `register_tool` ADDS a new tool; `replace_tool` takes over the EXECUTION
// of one that already exists. This demo wraps `list_dir`: every call is
// logged to the mod's persistent KV, the input is passed through (rewrite
// it in `delegate(#{...})` to translate arguments), and the original
// implementation produces the result.
//
// Metadata discipline: name/schema/description come from the spec map
// (here: keep the incumbent's), while capabilities/approval stay the
// incumbent's — a replacement can never relax an approval gate. The
// `tool-call` and `tool-result` waterfalls still run around the call.
//
// Try it: copy this directory to ~/.codesmith/mods/replace-tool/, activate
// with /mods activate replace-tool, list a directory from the next turn,
// then /calls to see the captured invocations.

replace_tool(#{ name: "list_dir" }, |input, ctx| {
    let n = mod_state_get("calls") ?? 0;
    mod_state_set("calls", n + 1);
    let who = if ctx.agent.main { "main" } else { ctx.agent.type };
    mod_log(`list_dir call #${n + 1} on ${input.path ?? "."} (${who})`);
    delegate(input)
});

register_command("calls", "Show the captured list_dir call count", |args| {
    message(`${mod_state_get("calls") ?? 0} list_dir calls captured`)
});
