use super::*;
use tempfile::tempdir;

fn make_assignment() -> SubAgentAssignment {
    SubAgentAssignment::new("prompt".to_string(), Some("worker".to_string()))
}

fn make_snapshot(status: SubAgentStatus) -> SubAgentResult {
    SubAgentResult {
        name: "agent_test".to_string(),
        agent_id: "agent_test".to_string(),
        context_mode: "fresh".to_string(),
        fork_context: false,
        agent_type: SubAgentType::General,
        assignment: make_assignment(),
        model: "deepseek-v4-flash".to_string(),
        nickname: None,
        status,
        agent_memory: None,
        result: None,
        steps_taken: 0,
        duration_ms: 0,
        from_prior_session: false,
    }
}

fn message_text(message: &Message) -> &str {
    match message.content.first() {
        Some(ContentBlock::Text { text, .. }) => text.as_str(),
        other => panic!("expected text content block, got {other:?}"),
    }
}

fn estimate_tool_description_tokens_conservative(text: &str) -> usize {
    text.chars().count().div_ceil(3)
}

/// A registry over the shared stub runtime, for `SubAgentToolRegistry`
/// construction tests (the stub never runs a turn).
fn make_test_registry(
    agent_type: SubAgentType,
    allowed_tools: Option<Vec<String>>,
) -> SubAgentToolRegistry {
    make_test_registry_with(agent_type, allowed_tools, false)
}

fn make_test_registry_with(
    agent_type: SubAgentType,
    allowed_tools: Option<Vec<String>>,
    children_read_only: bool,
) -> SubAgentToolRegistry {
    SubAgentToolRegistry::new(
        stub_runtime().with_children_read_only(children_read_only),
        agent_type,
        allowed_tools,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    )
}

#[test]
fn test_agent_type_from_str() {
    assert_eq!(
        SubAgentType::from_str("general"),
        Some(SubAgentType::General)
    );
    assert_eq!(
        SubAgentType::from_str("explore"),
        Some(SubAgentType::Explore)
    );
    assert_eq!(SubAgentType::from_str("PLAN"), Some(SubAgentType::Plan));
    assert_eq!(
        SubAgentType::from_str("code-review"),
        Some(SubAgentType::Review)
    );
    assert_eq!(
        SubAgentType::from_str("worker"),
        Some(SubAgentType::General)
    );
    assert_eq!(
        SubAgentType::from_str("default"),
        Some(SubAgentType::General)
    );
    assert_eq!(
        SubAgentType::from_str("explorer"),
        Some(SubAgentType::Explore)
    );
    assert_eq!(SubAgentType::from_str("awaiter"), Some(SubAgentType::Plan));
    assert_eq!(
        SubAgentType::from_str("tool-agent"),
        Some(SubAgentType::ToolAgent)
    );
    assert_eq!(SubAgentType::from_str("fin"), Some(SubAgentType::ToolAgent));
    assert_eq!(SubAgentType::from_str("invalid"), None);
}

#[test]
fn test_agent_type_implementer_aliases() {
    // #404 — Implementer accepts the obvious aliases the model is
    // likely to reach for when the user says "build this".
    for alias in ["implementer", "implement", "implementation", "builder"] {
        assert_eq!(
            SubAgentType::from_str(alias),
            Some(SubAgentType::Implementer),
            "alias {alias} should resolve to Implementer"
        );
    }
    // Case-insensitive.
    assert_eq!(
        SubAgentType::from_str("IMPLEMENTER"),
        Some(SubAgentType::Implementer)
    );
}

#[test]
fn test_agent_type_verifier_aliases() {
    // #404 — Verifier accepts test/validate aliases distinct from
    // Reviewer, which is for *grading* code rather than *running* it.
    for alias in ["verifier", "verify", "verification", "validator", "tester"] {
        assert_eq!(
            SubAgentType::from_str(alias),
            Some(SubAgentType::Verifier),
            "alias {alias} should resolve to Verifier"
        );
    }
    assert_eq!(
        SubAgentType::from_str("VERIFY"),
        Some(SubAgentType::Verifier)
    );
}

#[test]
fn test_agent_type_round_trips_via_as_str() {
    // Every type should serialize to a string that round-trips back
    // through `from_str`. Catches missed variants when adding a new
    // role.
    for t in [
        SubAgentType::General,
        SubAgentType::Explore,
        SubAgentType::Plan,
        SubAgentType::Review,
        SubAgentType::Implementer,
        SubAgentType::Verifier,
        SubAgentType::ToolAgent,
        SubAgentType::Custom,
    ] {
        let label = t.as_str();
        let back = SubAgentType::from_str(label)
            .unwrap_or_else(|| panic!("as_str label {label:?} doesn't round-trip via from_str"));
        assert_eq!(back, t, "round-trip failed for {t:?} via {label:?}");
    }
}

#[test]
fn test_implementer_and_verifier_have_distinct_prompts() {
    // The whole point of adding the types is that they carry distinct
    // posture. Defensive guard: catch the easy bug where copy-paste
    // leaves two new variants with the same prompt as `General`.
    let implementer = SubAgentType::Implementer.system_prompt();
    let verifier = SubAgentType::Verifier.system_prompt();
    let general = SubAgentType::General.system_prompt();
    assert_ne!(
        implementer, general,
        "Implementer prompt must differ from General"
    );
    assert_ne!(
        verifier, general,
        "Verifier prompt must differ from General"
    );
    assert_ne!(
        implementer, verifier,
        "Implementer and Verifier must differ"
    );
    // Sanity: each prompt mentions the role's defining verb so the
    // model has clear direction.
    assert!(
        implementer.to_lowercase().contains("implement")
            || implementer.to_lowercase().contains("write the code"),
        "Implementer prompt should reference its role: {implementer}"
    );
    assert!(
        verifier.to_lowercase().contains("verif")
            || verifier.to_lowercase().contains("test suite")
            || verifier.to_lowercase().contains("validation"),
        "Verifier prompt should reference its role: {verifier}"
    );
}

#[test]
fn test_agent_type_prompts_include_shared_output_contract_once() {
    for (agent_type, marker) in [
        (SubAgentType::General, "general-purpose sub-agent"),
        (SubAgentType::Explore, "exploration sub-agent"),
        (SubAgentType::Plan, "planning sub-agent"),
        (SubAgentType::Review, "code review sub-agent"),
        (SubAgentType::Implementer, "implementation sub-agent"),
        (SubAgentType::Verifier, "verification sub-agent"),
        (SubAgentType::ToolAgent, "tool execution sub-agent"),
        (SubAgentType::Custom, "custom sub-agent"),
    ] {
        let prompt = agent_type.system_prompt();
        assert!(prompt.contains(marker));
        assert_eq!(
            prompt.matches("## Output contract (mandatory)").count(),
            1,
            "{agent_type:?} prompt should include the shared output contract exactly once"
        );
        assert!(prompt.contains("### SUMMARY") && prompt.contains("### BLOCKERS"));
    }
}

#[test]
fn explore_prompt_orients_before_searching() {
    let prompt = SubAgentType::Explore.system_prompt();
    assert!(prompt.contains("role: `explore`"));
    assert!(prompt.contains("AGENTS.md/README"));
    assert!(prompt.contains("workspace/project root"));
    assert!(prompt.contains("compressed reconnaissance"));
}

#[test]
fn agent_open_description_explains_fresh_vs_forked_context_and_trust_model() {
    let tmp = tempdir().expect("tempdir");
    let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 1);
    let tool = AgentOpenTool::new(manager, stub_runtime());
    let description = tool.description();

    assert!(description.contains("fresh child with an independent prefill"));
    assert!(description.contains("fork_context=true"));
    assert!(description.contains("byte-identically"));
    assert!(description.contains("DeepSeek can reuse its prefix cache"));
    assert!(description.contains("Sub-agent results are self-reports"));
    assert!(
        estimate_tool_description_tokens_conservative(description) <= 1024,
        "agent_open description exceeds the conservative 1024-token budget"
    );
}

#[test]
fn new_session_tools_use_open_eval_close_names() {
    let manager = Arc::new(RwLock::new(SubAgentManager::new(PathBuf::from("."), 1)));
    assert_eq!(
        AgentOpenTool::new(manager.clone(), stub_runtime()).name(),
        "agent_open"
    );
    assert_eq!(AgentEvalTool::new(manager.clone()).name(), "agent_eval");
    assert_eq!(
        ToolAgentTool::new(manager.clone(), stub_runtime()).name(),
        "tool_agent"
    );
    assert_eq!(AgentCloseTool::new(manager).name(), "agent_close");
}

#[test]
fn tool_agent_description_explains_fast_lane() {
    let tmp = tempdir().expect("tempdir");
    let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 1);
    let tool = ToolAgentTool::new(manager, stub_runtime());
    let description = tool.description();

    assert!(description.contains("Fin"));
    // Provider-neutral routing: the light tier of the active provider, not
    // a hardcoded DeepSeek model name.
    assert!(description.contains("light tier"));
    assert!(!description.contains("Flash"));
    assert!(description.contains("thinking forced off"));
    assert!(description.contains("OCR"));
}

#[test]
fn test_implementer_allowed_tools_include_writes() {
    // Implementer is the write-heavy role; the deprecated
    // `allowed_tools()` advisory list should reflect that the role
    // can write/edit/patch even if today's runtime grants full
    // inheritance.
    #[allow(deprecated)]
    let tools = SubAgentType::Implementer.allowed_tools();
    assert!(tools.contains(&"write_file"));
    assert!(tools.contains(&"edit_file"));
    assert!(tools.contains(&"apply_patch"));
}

#[test]
fn test_verifier_allowed_tools_include_test_runner_but_no_writes() {
    // Verifier runs validation; it should not have write tools in
    // its advisory list. The runtime will still gate writes through
    // approval, but the advisory list signals intent.
    #[allow(deprecated)]
    let tools = SubAgentType::Verifier.allowed_tools();
    assert!(tools.contains(&"run_tests"));
    assert!(tools.contains(&"diagnostics"));
    assert!(!tools.contains(&"write_file"));
    assert!(!tools.contains(&"apply_patch"));
}

#[test]
fn test_parse_spawn_request_accepts_message_and_agent_type_aliases() {
    let input = json!({
        "message": "Find references to Foo",
        "agent_type": "explorer"
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert_eq!(parsed.prompt, "Find references to Foo");
    assert_eq!(parsed.agent_type, SubAgentType::Explore);
    assert_eq!(parsed.assignment.role.as_deref(), Some("explorer"));
}

#[test]
fn test_parse_spawn_request_accepts_objective_and_role_alias() {
    let input = json!({
        "objective": "Coordinate and wait",
        "role": "awaiter"
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert_eq!(parsed.prompt, "Coordinate and wait");
    assert_eq!(parsed.agent_type, SubAgentType::Plan);
    assert_eq!(parsed.assignment.role.as_deref(), Some("awaiter"));
}

#[test]
fn test_parse_spawn_request_accepts_items_payload() {
    let input = json!({
        "items": [
            {"type": "text", "text": "Analyze module"},
            {"type": "mention", "name": "drive", "path": "app://drive"}
        ],
        "agent_name": "explorer"
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert!(parsed.prompt.contains("Analyze module"));
    assert!(parsed.prompt.contains("[mention:$drive](app://drive)"));
    assert_eq!(parsed.agent_type, SubAgentType::Explore);
}

#[test]
fn test_parse_spawn_request_accepts_fork_context() {
    let input = json!({
        "prompt": "continue from here",
        "fork_context": true
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert!(parsed.fork_context);

    let input = json!({
        "prompt": "continue from here",
        "inherit_context": true
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert!(parsed.fork_context);
}

#[test]
fn test_parse_spawn_request_accepts_session_name_for_agent_open() {
    let input = json!({
        "name": "review.parser",
        "prompt": "inspect parser",
        "fork_context": true,
        "max_depth": 0
    });
    let parsed = parse_spawn_request(&input).expect("open request should parse");
    assert_eq!(parsed.session_name.as_deref(), Some("review.parser"));
    assert!(parsed.fork_context);
    assert_eq!(parsed.max_depth, Some(0));
}

#[test]
fn test_parse_spawn_request_accepts_tool_agent_aliases() {
    let input = json!({
        "prompt": "OCR this screenshot",
        "agent_type": "tool-agent"
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert_eq!(parsed.agent_type, SubAgentType::ToolAgent);
    assert_eq!(parsed.assignment.role.as_deref(), Some("tool_agent"));
}

#[test]
fn test_parse_spawn_request_rejects_invalid_session_name() {
    let input = json!({
        "name": "bad name",
        "prompt": "inspect parser"
    });
    let err = parse_spawn_request(&input).expect_err("space in name should fail");
    assert!(err.to_string().contains("name must not contain whitespace"));
}

#[test]
fn test_parse_spawn_request_rejects_out_of_range_max_depth() {
    let input = json!({
        "name": "review.parser",
        "prompt": "inspect parser",
        "max_depth": 4
    });
    let err = parse_spawn_request(&input).expect_err("max_depth should be capped at schema range");
    assert!(
        err.to_string()
            .contains("max_depth must be between 0 and 3")
    );
}

#[tokio::test]
async fn session_projection_exposes_forked_prefix_cache_contract() {
    let mut snapshot = make_snapshot(SubAgentStatus::Running);
    snapshot.name = "fanout_review".to_string();
    snapshot.context_mode = "forked".to_string();
    snapshot.fork_context = true;

    let ctx = ToolContext::new(".");
    let projection = subagent_session_projection(snapshot, false, &ctx).await;

    assert_eq!(projection.name, "fanout_review");
    assert_eq!(projection.context_mode, "forked");
    assert!(projection.fork_context);
    assert_eq!(projection.prefix_cache.mode, "forked");
    assert_eq!(
        projection.prefix_cache.parent_prefix,
        "preserved_byte_identical_when_available"
    );
    assert_eq!(projection.transcript_handle.kind, "var_handle");
    assert_eq!(projection.transcript_handle.name, "transcript");
}

#[tokio::test]
async fn terminal_session_projection_prefers_full_transcript_handle() {
    let mut snapshot = make_snapshot(SubAgentStatus::Completed);
    snapshot.result = Some("done".to_string());

    let ctx = ToolContext::new(".");
    let full_handle = {
        let mut store = ctx.runtime.handle_store.lock().await;
        store.insert_json(
            "agent:agent_test",
            "full_transcript",
            json!({
                "kind": "subagent_full_transcript",
                "agent_id": "agent_test",
                "messages": [
                    {
                        "role": "assistant",
                        "content": [
                            { "type": "text", "text": "complete child output" }
                        ]
                    }
                ]
            }),
        )
    };

    let projection = subagent_session_projection(snapshot, false, &ctx).await;

    assert_eq!(projection.transcript_handle, full_handle);
    assert_eq!(projection.transcript_handle.name, "full_transcript");
}

#[test]
fn test_parse_spawn_request_loads_custom_agent_definition_memory() {
    let tmp = tempdir().unwrap();
    let agents_dir = tmp.path().join(".codesmith").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("researcher.md"),
        r#"---
name: researcher
agent_type: explore
allowed_tools: [read_file, grep_files]
memory: project
---
Use the project research playbook.
"#,
    )
    .unwrap();

    let input = json!({
        "prompt": "Find parser notes",
        "agent_type": "researcher"
    });
    let parsed = parse_spawn_request_with_workspace(&input, tmp.path()).expect("definition parses");
    assert_eq!(parsed.agent_type, SubAgentType::Explore);
    assert_eq!(parsed.assignment.role.as_deref(), Some("researcher"));
    assert!(
        parsed
            .prompt
            .starts_with("Use the project research playbook.")
    );
    assert_eq!(
        parsed.allowed_tools,
        Some(vec!["read_file".to_string(), "grep_files".to_string()])
    );
    assert_eq!(parsed.memory.unwrap().scope, AgentMemoryScope::Project);
}

#[test]
fn test_delegate_defaults_to_fork_context() {
    let input = with_default_fork_context(json!({ "prompt": "review current work" }), true);
    let parsed = parse_spawn_request(&input).expect("delegate request should parse");
    assert!(parsed.fork_context);

    let input = with_default_fork_context(
        json!({ "prompt": "fresh exploration", "fork_context": false }),
        true,
    );
    let parsed = parse_spawn_request(&input).expect("delegate override should parse");
    assert!(!parsed.fork_context);
}

#[test]
fn forked_subagent_messages_preserve_parent_prefix_then_append_task() {
    let parent_system = SystemPrompt::Text("parent system".to_string());
    let parent_message = Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: "parent turn".to_string(),
            cache_control: None,
        }],
    };
    let fork_context = SubAgentForkContext {
        system: Some(parent_system.clone()),
        messages: vec![parent_message.clone()],
        structured_state_block: Some(
            "## Cycle State (Auto-Preserved)\n- Mode: `AGENT`".to_string(),
        ),
        current_assistant_text: None,
        current_turn_tool_calls: None,
    };

    let assignment = SubAgentAssignment::new("inspect parser".to_string(), Some("worker".into()));
    let messages = build_initial_subagent_messages(
        "inspect parser",
        &assignment,
        &SubAgentType::General,
        Some(&fork_context),
        None,
    );

    assert_eq!(
        subagent_request_system_prompt("child system", Some(&fork_context)),
        parent_system
    );
    assert_eq!(messages.first(), Some(&parent_message));
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1].role, "system");
    assert!(message_text(&messages[1]).contains("<codesmith:fork_state>"));
    assert_eq!(messages[2].role, "system");
    assert!(message_text(&messages[2]).contains("<codesmith:subagent_context>"));
    assert_eq!(messages[3].role, "user");
    assert!(message_text(&messages[3]).contains("inspect parser"));
}

#[test]
fn fresh_subagent_messages_keep_existing_single_turn_shape() {
    let assignment = SubAgentAssignment::new("list files".to_string(), None);
    let messages = build_initial_subagent_messages(
        "list files",
        &assignment,
        &SubAgentType::Explore,
        None,
        None,
    );

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    assert!(message_text(&messages[0]).contains("list files"));
}

#[test]
fn test_parse_spawn_request_rejects_text_and_items_together() {
    let input = json!({
        "prompt": "Analyze module",
        "items": [{"type": "text", "text": "dup"}]
    });
    let err = parse_spawn_request(&input).expect_err("text+items should fail");
    assert!(err.to_string().contains("either prompt text or items"));
}

#[test]
fn test_parse_spawn_request_rejects_invalid_role() {
    let input = json!({
        "prompt": "do work",
        "role": "unknown_role"
    });
    let err = parse_spawn_request(&input).expect_err("invalid role should fail");
    assert!(err.to_string().contains("Invalid role alias"));
}

#[test]
fn test_parse_spawn_request_rejects_conflicting_type_and_role() {
    let input = json!({
        "prompt": "inspect internals",
        "type": "explore",
        "role": "worker"
    });
    let err = parse_spawn_request(&input).expect_err("conflicting type+role should fail");
    assert!(
        err.to_string()
            .contains("Conflicting type/agent_type and role/agent_role")
    );
}

#[test]
fn test_parse_assign_request_accepts_aliases() {
    let input = json!({
        "id": "agent_1234",
        "objective": "re-check failing tests",
        "agent_role": "explorer",
        "input": "focus on tests only",
        "interrupt": false
    });
    let request = parse_assign_request(&input).expect("assign request should parse");
    assert_eq!(request.agent_id, "agent_1234");
    assert_eq!(request.objective.as_deref(), Some("re-check failing tests"));
    assert_eq!(request.role.as_deref(), Some("explorer"));
    assert_eq!(request.message.as_deref(), Some("focus on tests only"));
    assert!(!request.interrupt);
}

#[test]
fn test_parse_assign_request_rejects_invalid_role() {
    let input = json!({
        "agent_id": "agent_1234",
        "role": "unknown"
    });
    let err = parse_assign_request(&input).expect_err("invalid role should fail");
    assert!(err.to_string().contains("Invalid role alias"));
}

#[test]
fn test_parse_assign_request_requires_update_fields() {
    let input = json!({
        "agent_id": "agent_1234"
    });
    let err = parse_assign_request(&input).expect_err("missing update fields should fail");
    assert!(
        err.to_string().contains(
            "Provide at least one of objective, role/agent_role, message/input, or items"
        )
    );
}

#[test]
fn test_build_allowed_tools_independent_of_allow_shell() {
    // v0.6.6: allow_shell no longer filters at the build_allowed_tools
    // level — the registry builder controls shell-tool registration.
    // Both calls return None (full inheritance) for a default General
    // agent.
    let with_shell = build_allowed_tools(&SubAgentType::General, None, true, None, false).unwrap();
    let without_shell =
        build_allowed_tools(&SubAgentType::General, None, false, None, false).unwrap();
    assert!(with_shell.is_none());
    assert!(without_shell.is_none());
}

#[test]
fn test_allowed_tools_are_deduplicated() {
    let tools = build_allowed_tools(
        &SubAgentType::Custom,
        Some(vec![
            "read_file".to_string(),
            "read_file".to_string(),
            "  ".to_string(),
            "grep_files".to_string(),
        ]),
        true,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        tools,
        Some(vec!["read_file".to_string(), "grep_files".to_string()])
    );
}

#[test]
fn test_custom_agent_requires_allowed_tools() {
    let err = build_allowed_tools(&SubAgentType::Custom, None, true, None, false).unwrap_err();
    assert!(err.to_string().contains("requires"));
}

#[test]
fn test_wait_mode_condition_any_and_all() {
    let one_done = vec![
        make_snapshot(SubAgentStatus::Running),
        make_snapshot(SubAgentStatus::Completed),
    ];
    let all_done = vec![
        make_snapshot(SubAgentStatus::Completed),
        make_snapshot(SubAgentStatus::Cancelled),
    ];

    assert!(WaitMode::Any.condition_met(&one_done));
    assert!(!WaitMode::All.condition_met(&one_done));
    assert!(WaitMode::All.condition_met(&all_done));
}

#[test]
fn test_parse_wait_mode() {
    assert_eq!(parse_wait_mode(&json!({})).unwrap(), WaitMode::Any);
    assert_eq!(
        parse_wait_mode(&json!({"wait_mode": "all"})).unwrap(),
        WaitMode::All
    );
    assert_eq!(
        parse_wait_mode(&json!({"wait_mode": "first"})).unwrap(),
        WaitMode::Any
    );
    assert!(parse_wait_mode(&json!({"wait_mode": "invalid"})).is_err());
}

#[test]
fn test_parse_wait_ids_accepts_aliases() {
    let ids = parse_wait_ids(&json!({
        "ids": ["agent_a", "agent_b"],
        "agent_id": "agent_c",
        "id": "agent_a"
    }));

    assert_eq!(ids, vec!["agent_a", "agent_b", "agent_c"]);
}

#[test]
fn test_parse_wait_ids_empty_when_omitted() {
    let ids = parse_wait_ids(&json!({}));
    assert!(ids.is_empty());
}

#[test]
fn test_build_assignment_prompt_includes_metadata() {
    let assignment = SubAgentAssignment::new(
        "Inspect parser behavior".to_string(),
        Some("explorer".to_string()),
    );
    let prompt = build_assignment_prompt(
        "Inspect parser behavior",
        &assignment,
        &SubAgentType::Explore,
    );
    assert!(prompt.contains("Assignment metadata"));
    assert!(prompt.contains("resolved_type: explore"));
    assert!(prompt.contains("role: explorer"));
}

#[test]
fn subagent_auto_model_routes_unconfigured_assignments() {
    // No tier pair configured (embedders/tests: `AutoRouteContext::default`
    // carries `tier_models: None`): tier routing falls back to the
    // runtime's main model instead of vendor-specific default IDs that may
    // not exist on the active provider.
    let runtime = stub_runtime().with_auto_model(true);

    assert_eq!(
        fallback_subagent_assignment_route(&runtime, None, "implement the release fix").model,
        runtime.model
    );
    assert_eq!(
        fallback_subagent_assignment_route(&runtime, None, "say hello").model,
        runtime.model
    );
}

#[test]
fn subagent_auto_route_respects_explicit_or_role_model() {
    let runtime = stub_runtime().with_auto_model(true);

    assert_eq!(
        fallback_subagent_assignment_route(
            &runtime,
            Some("deepseek-v4-flash".to_string()),
            "implement the release fix"
        )
        .model,
        "deepseek-v4-flash"
    );
}

#[tokio::test]
async fn tool_agent_route_forces_flash_with_thinking_off() {
    let runtime = stub_runtime()
        .with_auto_model(false)
        .with_reasoning_effort(Some("max".to_string()), false);

    let route = resolve_subagent_assignment_route(
        &runtime,
        Some("deepseek-v4-pro".to_string()),
        "run OCR on this screenshot",
        &SubAgentType::ToolAgent,
    )
    .await;

    assert_eq!(route.model, "deepseek-v4-flash");
    assert_eq!(route.reasoning_effort.as_deref(), Some("off"));
}

#[test]
fn subagent_auto_reasoning_resolves_to_distinct_v4_tiers() {
    let runtime = stub_runtime().with_reasoning_effort(Some("high".to_string()), true);

    assert_eq!(
        fallback_subagent_assignment_route(&runtime, None, "quick lookup").reasoning_effort,
        Some("high".to_string())
    );
    assert_eq!(
        fallback_subagent_assignment_route(&runtime, None, "debug this release failure")
            .reasoning_effort,
        Some("max".to_string())
    );
}

#[test]
fn fixed_model_subagent_auto_reasoning_skips_router() {
    let runtime = stub_runtime().with_reasoning_effort(Some("high".to_string()), true);

    assert!(
        !should_use_subagent_router(&runtime),
        "fixed-model auto thinking should resolve locally without a hidden router request"
    );
}

#[test]
fn auto_model_subagent_assignments_still_use_router() {
    let runtime = stub_runtime().with_auto_model(true);

    assert!(
        should_use_subagent_router(&runtime),
        "auto-model sub-agent assignments still need router guidance"
    );
}

#[test]
fn subagent_router_prompt_frames_assignment_as_auto_routing() {
    let runtime = stub_runtime()
        .with_auto_model(true)
        .with_reasoning_effort(Some("high".to_string()), true);
    let prompt = subagent_router_prompt(&runtime, "inspect one file");

    assert!(prompt.contains("Parent selected model mode: auto"));
    assert!(prompt.contains("Parent selected thinking mode: auto"));
    assert!(prompt.contains("inspect one file"));
}

#[test]
fn test_subagent_tool_registry_reports_unavailable_tools() {
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.allow_shell = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Explore,
        Some(vec!["read_file".to_string(), "missing_tool".to_string()]),
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    assert_eq!(
        registry.unavailable_allowed_tools(),
        vec!["missing_tool".to_string()]
    );
}

#[test]
fn test_review_agent_tools_exclude_agent_spawn() {
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    // None = full parent tool inheritance (the default for builtin types).
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Review,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let tools = registry.tools_for_model(&SubAgentType::Review);
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        !names.contains(&"agent_spawn"),
        "Review agent must not have agent_spawn; tools: {names:?}"
    );
}

// === §F5d T4 §4b — subagent ext-tool exclusion regression =================
//
// `codesmith_agent::extension` traits are not in `super::*` (the subagent
// module doesn't import them); pull in just what the stub needs. `ToolResult`
// IS already in scope via `super::*` (re-export of `codesmith_tools::ToolResult`).
use codesmith_agent::extension::{
    Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext, ExtensionError,
    ExtensionMetadata, ExtensionMode, ToolDefinition,
};

/// Stub extension that registers a single tool named `fixture_echo` — the
/// canonical ext-tool name (same name the fixture cdylib registers; the
/// cdylib's path is only emitted by `codesmith-extensions/build.rs`, so the
/// CLI can't load it directly — this stub is the closest feasible analog for the
/// §4b regression). Used ONLY to put a concrete ext tool on a parent
/// `ExtensionRunner` so the §4b test can assert it does NOT transit to a
/// sub-agent built without that runner.
struct StubFixtureEchoExt;

#[async_trait]
impl Extension for StubFixtureEchoExt {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("stub-fixture-echo");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(StubFixtureEchoTool))?;
        Ok(())
    }
}

struct StubFixtureEchoTool;

#[async_trait]
impl ToolDefinition for StubFixtureEchoTool {
    fn name(&self) -> &str {
        "fixture_echo"
    }
    fn description(&self) -> &str {
        "Stub fixture echo tool (§4b regression)."
    }
    async fn execute(
        &self,
        _input: Value,
        _ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError> {
        Ok(ToolResult::success("stub"))
    }
}

/// Minimal `ExtensionContext` + `ExtensionCommandContext` for `bind_core` in
/// the §4b test (mirrors the `Ctx` in `codesmith-extensions` runner tests).
/// `ExtensionContext` is `#[async_trait]` (its action methods have defaults
/// returning `Unimplemented`); only the 5 observation methods are required.
struct StubExtCtx;

#[async_trait]
impl ExtensionContext for StubExtCtx {
    fn cwd(&self) -> &Path {
        Path::new(".")
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Interactive
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn signal(&self) -> CancellationToken {
        CancellationToken::new()
    }
    fn generation(&self) -> u64 {
        0
    }
}
impl ExtensionCommandContext for StubExtCtx {}

/// §F5d T4 §4b → 5b restatement — a sub-agent never holds a dylib `Arc`.
/// The exclusion is now per-ORIGIN, not per-surface:
///   1. `SubAgentToolRegistry::new` mounts the child type's extension-tool
///      view via `register_extension_tools_scoped`, which resolves
///      `ExtensionRunner::mountable_tools_for_subagents` — dylib-origin
///      tools are filtered out there (a detached background child can
///      outlive a reload's two-phase `Library` drop; §F5d T4's safety
///      argument covers the main-turn executor only);
///   2. script/compiled-in origins hold statically linked code or retained
///      script ASTs and mount freely.
///
/// So a dylib-origin ext tool NEVER reaches a sub-agent's effective set —
/// even with that very runner attached to the child runtime — regardless
/// of `inherit_full_registry`; a compiled-in ext tool DOES mount (the
/// blanket exclusion was the pre-5b contract this test used to pin).
///
/// EXPECTED GREEN. If the dylib half goes Red, dylib `Arc`s are leaking
/// into sub-agents — STOP + report.
#[test]
fn subagent_ext_tool_origin_tiering() {
    let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
    let rt = tokio::runtime::Runtime::new().expect("rt");
    // A compiled-in contribution (plain `load` stamps CompiledIn)…
    rt.block_on(runner.load(&StubFixtureEchoExt))
        .expect("load compiled-in stub");
    // …and a dylib-stamped contribution (same shape, no real dylib needed:
    // the origin stamp is what the tier keys on).
    rt.block_on(runner.load_as(
        &StubDylibEchoExt,
        codesmith_extensions::RegistrationOrigin::Dylib,
    ))
    .expect("load dylib-stamped stub");
    runner.bind_core(Arc::new(StubExtCtx));
    // Precondition: BOTH are bound on the Global view (the main turn sees
    // them equally — tiering only applies to sub-agent surfaces).
    let global: Vec<String> = runner.bound_tools().into_iter().map(|(n, _)| n).collect();
    assert!(
        global.contains(&"fixture_echo".to_string()) && global.contains(&"dylib_echo".to_string()),
        "both stubs bound on the parent runner (precondition): {global:?}"
    );

    let tmp = tempdir().expect("tempdir");

    // Both inheritance modes: compiled-in mounts, dylib stays out.
    for inherit in [false, true] {
        let mut runtime = stub_runtime()
            .with_inherit_full_registry(inherit)
            .with_extension_runner(Some(runner.clone()));
        runtime.context = ToolContext::new(tmp.path().to_path_buf());
        let registry = SubAgentToolRegistry::new(
            runtime,
            SubAgentType::General,
            None,
            Arc::new(Mutex::new(TodoList::new())),
            Arc::new(Mutex::new(PlanState::default())),
        );
        let tools = registry.tools_for_model(&SubAgentType::General);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"fixture_echo"),
            "compiled-in ext tool mounts in the child (inherit={inherit}): {names:?}"
        );
        assert!(
            !names.contains(&"dylib_echo"),
            "dylib-origin ext tool stays main-turn-only (inherit={inherit}): {names:?}"
        );
    }
}

/// A dylib-stamped stub registering `dylib_echo` — the tier fixture for
/// `subagent_ext_tool_origin_tiering` (the origin stamp is the whole
/// difference from `StubFixtureEchoExt`).
struct StubDylibEchoExt;

#[async_trait]
impl Extension for StubDylibEchoExt {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("stub-dylib-echo");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        struct DylibEcho;
        #[async_trait]
        impl ToolDefinition for DylibEcho {
            fn name(&self) -> &str {
                "dylib_echo"
            }
            fn description(&self) -> &str {
                "Dylib-stamped fixture echo tool."
            }
            async fn execute(
                &self,
                _input: Value,
                _ctx: &dyn ExtensionContext,
            ) -> Result<ToolResult, ExtensionError> {
                Ok(ToolResult::success("stub"))
            }
        }
        api.register_tool(Box::new(DylibEcho))?;
        Ok(())
    }
}

/// 5b — scoped registrations resolve per agent type in the child registry:
/// a `Global` tool mounts for every type, an `AgentType("explore")` tool
/// mounts only for explore children (shadowing a same-name global
/// registration there), and the explore read-only narrowing admits ext
/// tools through their declared `ReadOnly` capability.
#[test]
fn subagent_scoped_ext_tool_resolves_per_agent_type() {
    struct ReadOnlyTool(&'static str, &'static str);
    #[async_trait]
    impl ToolDefinition for ReadOnlyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            self.1
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, ExtensionError> {
            Ok(ToolResult::success(self.1))
        }
    }
    struct ScopedToolsExt;
    #[async_trait]
    impl Extension for ScopedToolsExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("stub-scoped-tools");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            use codesmith_agent::extension::Scope;
            api.register_tool(Box::new(ReadOnlyTool("shared_ext", "global-variant")))?;
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(ReadOnlyTool("shared_ext", "explore-variant")),
            )?;
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(ReadOnlyTool("explore_ext", "explore fixture")),
            )?;
            Ok(())
        }
    }

    let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(runner.load(&ScopedToolsExt))
        .expect("load scoped stub");
    runner.bind_core(Arc::new(StubExtCtx));

    let tmp = tempdir().expect("tempdir");

    // General child: the global variant mounts; explore-scoped tools do not.
    let mut runtime = stub_runtime().with_extension_runner(Some(runner.clone()));
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let names: Vec<String> = registry
        .tools_for_model(&SubAgentType::General)
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"shared_ext".to_string()),
        "global ext tool mounts for general: {names:?}"
    );
    assert!(
        !names.contains(&"explore_ext".to_string()),
        "explore-scoped tool stays out of a general child: {names:?}"
    );
    assert_eq!(
        registry.registry.get("shared_ext").unwrap().description(),
        "global-variant",
        "the general child resolves the global registration"
    );

    // Explore child: the scoped shadow wins the shared name, the
    // explore-only tool appears (ReadOnly capability admits both through
    // the read-only narrowing), and same-name resolution picks the
    // scoped registration.
    let mut runtime = stub_runtime().with_extension_runner(Some(runner));
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Explore,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let names: Vec<String> = registry
        .tools_for_model(&SubAgentType::Explore)
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"explore_ext".to_string()),
        "explore-scoped tool mounts for an explore child: {names:?}"
    );
    assert!(
        names.contains(&"shared_ext".to_string()),
        "global ext tool survives the read-only narrowing via its ReadOnly \
         capability: {names:?}"
    );
    assert_eq!(
        registry.registry.get("shared_ext").unwrap().description(),
        "explore-variant",
        "the scoped registration shadows the global one in its own view"
    );
}

// === Capability manifest on the sub-agent path (slice 2b) =================
//
// `capabilities.toml [tools] disabled` is a session-level baseline. The
// sub-agent registry build is a separate path from `build_turn_dispatcher`;
// without wiring the disabled set into `SubAgentRuntime`, a child would
// resurrect a tool the user deliberately disabled — the unsafe direction
// the manifest exists to close.

#[test]
fn subagent_capability_disabled_tools_neither_visible_nor_registered() {
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime()
        .with_capability_disabled_tools(["web_search".to_string()].into_iter().collect());
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let tools = registry.tools_for_model(&SubAgentType::General);
    assert!(
        !tools.iter().any(|t| t.name == "web_search"),
        "manifest-disabled tool must not be model-visible in a sub-agent: {:?}",
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
    );
    assert!(
        !registry.registry.contains("web_search"),
        "manifest-disabled tool must not stay registered (executable) in a sub-agent"
    );
    assert!(
        registry.registry.contains("read_file"),
        "untouched tools stay registered (precondition)"
    );
}

#[test]
fn subagent_capability_disabled_explicit_request_fails_loud_with_attribution() {
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime()
        .with_capability_disabled_tools(["web_search".to_string()].into_iter().collect());
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Custom,
        Some(vec!["web_search".to_string(), "read_file".to_string()]),
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    // The disabled name surfaces as unavailable (spawn fails loud) and is
    // attributable to the manifest, not to a typo'd tool name.
    assert_eq!(registry.unavailable_allowed_tools(), vec!["web_search"]);
    assert!(registry.is_capability_disabled("web_search"));
    assert!(!registry.is_capability_disabled("read_file"));
}

#[test]
fn subagent_capability_disabled_set_propagates_to_descendants() {
    let runtime = stub_runtime()
        .with_capability_disabled_tools(["web_search".to_string()].into_iter().collect());
    assert!(
        runtime
            .child_runtime()
            .capability_disabled_tools
            .contains("web_search"),
        "child_runtime must carry the disabled set (grandchildren re-apply it)"
    );
    assert!(
        runtime
            .background_runtime()
            .capability_disabled_tools
            .contains("web_search"),
        "background_runtime must carry the disabled set (agent_open children)"
    );
}

#[tokio::test]
async fn test_wait_for_result_reports_timeout_when_still_running() {
    let manager = Arc::new(RwLock::new(SubAgentManager::new(PathBuf::from("."), 2)));
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let agent = SubAgent::new(
        "test_agent_1".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    let agent_id = agent.id.clone();
    {
        let mut guard = manager.write().await;
        guard.agents.insert(agent_id.clone(), agent);
    }

    let (snapshot, timed_out) = wait_for_result(&manager, &agent_id, Duration::from_millis(10))
        .await
        .expect("wait_for_result should succeed");
    assert!(timed_out);
    assert_eq!(snapshot.status, SubAgentStatus::Running);
}

// Regression for #1738: agent_eval on a terminated session must not
// hard-fail with "not running" when a follow-up message is supplied. The
// parent still needs the projection (and its transcript_handle) to recover
// the child's full output.
#[tokio::test]
async fn agent_eval_on_completed_session_returns_full_projection_not_running_error() {
    let manager = Arc::new(RwLock::new(SubAgentManager::new(PathBuf::from("."), 1)));
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_2".to_string(),
        SubAgentType::Explore,
        "analyze 14 issues".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    let full_output = format!("Per-issue analysis:\n{}", "detail line\n".repeat(400));
    agent.status = SubAgentStatus::Completed;
    agent.result = Some(full_output.clone());
    let agent_id = agent.id.clone();
    {
        let mut guard = manager.write().await;
        guard.agents.insert(agent_id.clone(), agent);
    }

    let ctx = ToolContext::new(".");
    let tool = AgentEvalTool::new(manager.clone());
    let result = tool
        .execute(
            json!({
                "agent_id": agent_id,
                "message": "give me the full per-issue breakdown",
                "block": false
            }),
            &ctx,
        )
        .await
        .expect("agent_eval on a completed session must not error");

    let meta = result.metadata.expect("metadata present");
    assert_eq!(meta["terminal"], json!(true));
    assert_eq!(meta["message_delivery"]["delivered"], json!(false));

    let projection: SubAgentSessionProjection =
        serde_json::from_str(&result.content).expect("projection deserializes");
    assert_eq!(projection.status, "completed");
    assert_eq!(projection.transcript_handle.kind, "var_handle");
    // The full, untruncated child output survives in the snapshot the
    // transcript_handle points at.
    assert_eq!(
        projection.snapshot.result.as_deref(),
        Some(full_output.as_str())
    );
}

#[tokio::test]
async fn test_running_count_counts_only_agents_with_live_task_handles() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_3".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Running;
    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    agent.task_handle = Some(handle);
    let agent_id = agent.id.clone();
    manager.agents.insert(agent.id.clone(), agent);

    assert_eq!(manager.running_count(), 1);
    manager
        .agents
        .get_mut(&agent_id)
        .and_then(|agent| agent.task_handle.take())
        .expect("live task handle")
        .abort();
}

#[tokio::test]
async fn test_live_running_snapshots_return_only_agents_with_live_task_handles() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 2);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut live_agent = SubAgent::new(
        "test_agent_live".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    live_agent.status = SubAgentStatus::Running;
    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    live_agent.task_handle = Some(handle);
    let live_agent_id = live_agent.id.clone();
    manager.agents.insert(live_agent_id.clone(), live_agent);

    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut stale_agent = SubAgent::new(
        "test_agent_stale".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Green".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    stale_agent.status = SubAgentStatus::Running;
    manager.agents.insert(stale_agent.id.clone(), stale_agent);

    let snapshots = manager.live_running_snapshots();

    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].agent_id, live_agent_id);
    manager
        .agents
        .get_mut(&live_agent_id)
        .and_then(|agent| agent.task_handle.take())
        .expect("live task handle")
        .abort();
}

#[test]
fn test_running_count_ignores_running_status_without_task_handle() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_4".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Running;
    manager.agents.insert(agent.id.clone(), agent);

    assert_eq!(manager.running_count(), 0);
}

#[tokio::test]
async fn test_running_count_ignores_finished_task_handles() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_5".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Running;
    let handle = tokio::spawn(async {});
    handle.await.expect("dummy task should finish immediately");
    agent.task_handle = Some(tokio::spawn(async {}));
    if let Some(handle) = agent.task_handle.as_ref() {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    }
    manager.agents.insert(agent.id.clone(), agent);

    assert_eq!(manager.running_count(), 0);
}

#[test]
fn test_assign_updates_running_agent_and_sends_message() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 2);
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let agent = SubAgent::new(
        "test_agent_6".to_string(),
        SubAgentType::General,
        "work".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    let agent_id = agent.id.clone();
    manager.agents.insert(agent_id.clone(), agent);

    let snapshot = manager
        .assign(
            &agent_id,
            Some("Re-check module boundaries".to_string()),
            Some("explorer".to_string()),
            None,
            true,
        )
        .expect("assignment should succeed");
    assert_eq!(snapshot.assignment.objective, "Re-check module boundaries");
    assert_eq!(snapshot.assignment.role.as_deref(), Some("explorer"));

    let dispatched = input_rx
        .try_recv()
        .expect("running agent should receive assignment update");
    assert!(dispatched.interrupt);
    assert!(dispatched.text.contains("Assignment updated"));
    assert!(dispatched.text.contains("objective"));
}

#[test]
fn test_assign_rejects_message_for_non_running_agent() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_7".to_string(),
        SubAgentType::Explore,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Completed;
    let agent_id = agent.id.clone();
    manager.agents.insert(agent_id.clone(), agent);

    let err = manager
        .assign(&agent_id, None, None, Some("keep going".to_string()), true)
        .expect_err("non-running agent cannot receive assignment message");
    assert!(err.to_string().contains("is not running"));
}

#[test]
fn test_assign_updates_non_running_metadata_without_message() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "test_agent_8".to_string(),
        SubAgentType::Plan,
        "prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Completed;
    let agent_id = agent.id.clone();
    manager.agents.insert(agent_id.clone(), agent);

    let snapshot = manager
        .assign(
            &agent_id,
            Some("Draft retry plan".to_string()),
            Some("awaiter".to_string()),
            None,
            true,
        )
        .expect("metadata update should succeed");
    assert_eq!(snapshot.assignment.objective, "Draft retry plan");
    assert_eq!(snapshot.assignment.role.as_deref(), Some("awaiter"));
}

#[test]
fn test_persist_and_reload_marks_running_agent_as_interrupted() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let state_path = default_state_path(tmp.path());

    let mut manager = SubAgentManager::new(workspace.clone(), 2).with_state_path(state_path);
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let running = SubAgent::new(
        "test_agent_9_running".to_string(),
        SubAgentType::General,
        "work".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        Some("Blue".to_string()),
        Some(vec!["read_file".to_string()]),
        input_tx,
        "boot_test".to_string(),
    );
    let running_id = running.id.clone();
    manager.agents.insert(running_id.clone(), running);
    manager.persist_state().expect("persist state");

    let mut reloaded =
        SubAgentManager::new(workspace, 2).with_state_path(default_state_path(tmp.path()));
    reloaded.load_state().expect("load state");
    let snapshot = reloaded
        .get_result(&running_id)
        .expect("reloaded agent should exist");
    assert!(matches!(
        snapshot.status,
        SubAgentStatus::Interrupted(ref message)
            if message.contains(SUBAGENT_RESTART_REASON)
    ));
}

#[test]
fn test_interrupted_status_name_and_summary() {
    let snapshot = make_snapshot(SubAgentStatus::Interrupted(
        SUBAGENT_RESTART_REASON.to_string(),
    ));
    assert_eq!(subagent_status_name(&snapshot.status), "interrupted");
    assert!(summarize_subagent_result(&snapshot).contains(SUBAGENT_RESTART_REASON));
}

// === Finding F4 / plan 04 §4.1 — shared-state回流 via Arc ===

/// A child's mutation to a shared `Arc<Mutex<…>>` handle is atomically
/// visible to the parent with no explicit回流 call. This is the回流
/// equivalent of Claude Code's `contextModifier` for shared state — the
/// `Shared*` handles (`SharedTodoList`, `SharedPlanState`, …) are cloned by
/// `Arc::clone` at spawn, so parent and child mutate the same allocation.
#[test]
fn child_todo_mutation_is_visible_to_parent_via_shared_arc() {
    use crate::tools::todo::{SharedTodoList, TodoList, TodoStatus};

    // The parent holds the shared handle; the child clones the Arc.
    let parent_todo: SharedTodoList = Arc::new(Mutex::new(TodoList::new()));
    let child_todo = Arc::clone(&parent_todo);

    // The child mutates the shared list — no return channel, no queue.
    {
        let mut child_list = child_todo.blocking_lock();
        child_list.add(
            "land the parser refactor".to_string(),
            TodoStatus::InProgress,
        );
    }

    // The parent observes the mutation immediately, through its own clone.
    let snapshot = {
        let parent_list = parent_todo.blocking_lock();
        parent_list.snapshot()
    };
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].content, "land the parser refactor");
    assert_eq!(snapshot.items[0].status, TodoStatus::InProgress);
}

// === v0.6.6 — sub-agent authority unification ===

#[test]
fn build_allowed_tools_general_returns_none_for_full_inheritance() {
    // Default behavior: General agent with no explicit list inherits the
    // parent's full registry (None signals no narrowing).
    let result = build_allowed_tools(&SubAgentType::General, None, true, None, false).unwrap();
    assert!(
        result.is_none(),
        "General with no explicit_tools should default to full inheritance (None), got {result:?}"
    );
}

#[test]
fn build_allowed_tools_explore_returns_none_for_full_inheritance() {
    // This helper is advisory for every type — Explore's read-only surface is
    // enforced downstream, in `SubAgentToolRegistry::new`
    // (`explore_registry_narrows_to_read_only_tools` locks it).
    let result = build_allowed_tools(&SubAgentType::Explore, None, true, None, false).unwrap();
    assert!(
        result.is_none(),
        "Explore with no explicit_tools should default to full inheritance"
    );
}

#[test]
fn build_allowed_tools_custom_requires_explicit_list() {
    // Custom is the one type that REQUIRES explicit allowed_tools.
    let err = build_allowed_tools(&SubAgentType::Custom, None, true, None, false).unwrap_err();
    assert!(
        err.to_string().contains("Custom sub-agent requires"),
        "got: {err}"
    );
}

#[test]
fn build_allowed_tools_explicit_list_returned_as_some() {
    let explicit = vec!["read_file".to_string(), "list_dir".to_string()];
    let result = build_allowed_tools(
        &SubAgentType::Custom,
        Some(explicit.clone()),
        true,
        None,
        false,
    )
    .unwrap();
    assert_eq!(result, Some(explicit));
}

#[test]
fn build_allowed_tools_explicit_list_dedupes_and_trims() {
    let explicit = vec![
        "read_file".to_string(),
        "  read_file  ".to_string(), // trim + dedupe
        "list_dir".to_string(),
        "".to_string(), // skip empty
    ];
    let result =
        build_allowed_tools(&SubAgentType::Custom, Some(explicit), true, None, false).unwrap();
    assert_eq!(
        result,
        Some(vec!["read_file".to_string(), "list_dir".to_string()])
    );
}

// === Plan 04 / finding F4 — restrictToSubset (child ⊆ parent effective) ===

#[test]
fn build_allowed_tools_default_inherits_parent_basis_when_narrowed() {
    // A default General child of a NARROWED parent inherits the parent's
    // effective set — it cannot re-expand to the full surface.
    let basis = vec!["read_file".to_string(), "grep_files".to_string()];
    let result = build_allowed_tools(
        &SubAgentType::General,
        None,
        true,
        Some(basis.as_slice()),
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        Some(vec!["read_file".to_string(), "grep_files".to_string()])
    );
}

#[test]
fn build_allowed_tools_default_unrestricted_when_parent_basis_none() {
    // An unrestricted parent (full surface, basis = None) → General child
    // gets full inheritance (Ok(None)). This is the common top-level case,
    // so recursion via `agent_spawn` is preserved.
    let result = build_allowed_tools(&SubAgentType::General, None, true, None, false).unwrap();
    assert!(result.is_none());
}

#[test]
fn build_allowed_tools_explicit_intersected_with_parent_basis() {
    // A Custom child requesting [a, b, c] from a parent that only exposes
    // [a, b] is narrowed to [a, b] — no escalation.
    let basis = vec!["read_file".to_string(), "grep_files".to_string()];
    let result = build_allowed_tools(
        &SubAgentType::Custom,
        Some(vec![
            "read_file".to_string(),
            "grep_files".to_string(),
            "exec_shell".to_string(),
        ]),
        true,
        Some(basis.as_slice()),
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        Some(vec!["read_file".to_string(), "grep_files".to_string()])
    );
}

#[test]
fn build_allowed_tools_explicit_kept_when_parent_unrestricted() {
    // Unrestricted parent (basis None) → a Custom child's explicit list is
    // kept in full.
    let result = build_allowed_tools(
        &SubAgentType::Custom,
        Some(vec!["read_file".to_string(), "list_dir".to_string()]),
        true,
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        result,
        Some(vec!["read_file".to_string(), "list_dir".to_string()])
    );
}

#[test]
fn build_allowed_tools_escape_hatch_skips_default_subset() {
    // inherit_full_registry = true restores the legacy v0.6.6 default: even
    // with a narrowed parent basis, a default General child gets full
    // inheritance (Ok(None)).
    let basis = vec!["read_file".to_string()];
    let result = build_allowed_tools(
        &SubAgentType::General,
        None,
        true,
        Some(basis.as_slice()),
        true,
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn build_allowed_tools_escape_hatch_skips_explicit_intersection() {
    // Under the escape hatch, a Custom child's explicit list is NOT
    // intersected with the parent basis.
    let basis = vec!["read_file".to_string()];
    let result = build_allowed_tools(
        &SubAgentType::Custom,
        Some(vec![
            "read_file".to_string(),
            "grep_files".to_string(),
            "exec_shell".to_string(),
        ]),
        true,
        Some(basis.as_slice()),
        true,
    )
    .unwrap();
    assert_eq!(
        result,
        Some(vec![
            "read_file".to_string(),
            "grep_files".to_string(),
            "exec_shell".to_string()
        ])
    );
}

#[test]
fn build_allowed_tools_custom_empty_with_basis_still_errors() {
    // The Custom-requires-non-empty check fires before intersection, so an
    // empty explicit list still errors regardless of the parent basis.
    let basis = vec!["read_file".to_string()];
    let err = build_allowed_tools(
        &SubAgentType::Custom,
        Some(vec![]),
        true,
        Some(basis.as_slice()),
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("requires"));
}

#[test]
fn intersect_tool_names_none_basis_keeps_all() {
    let kept = intersect_tool_names(
        vec!["read_file".to_string(), "grep_files".to_string()],
        None,
    );
    assert_eq!(
        kept,
        vec!["read_file".to_string(), "grep_files".to_string()]
    );
}

#[test]
fn intersect_tool_names_some_basis_filters_out_missing() {
    let basis = vec!["read_file".to_string(), "list_dir".to_string()];
    let kept = intersect_tool_names(
        vec![
            "read_file".to_string(),
            "exec_shell".to_string(),
            "list_dir".to_string(),
        ],
        Some(basis.as_slice()),
    );
    assert_eq!(kept, vec!["read_file".to_string(), "list_dir".to_string()]);
}

#[test]
fn intersect_tool_names_preserves_request_order() {
    // The child's requested order is preserved; only membership is filtered.
    let basis = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let kept = intersect_tool_names(
        vec![
            "c".to_string(),
            "a".to_string(),
            "b".to_string(),
            "z".to_string(),
        ],
        Some(basis.as_slice()),
    );
    assert_eq!(
        kept,
        vec!["c".to_string(), "a".to_string(), "b".to_string()]
    );
}

#[test]
fn child_runtime_propagates_inherit_full_registry_and_basis() {
    // The escape-hatch flag propagates to children; the subset basis also
    // propagates so a grandchild's build_allowed_tools sees its parent's
    // effective set at spawn entry.
    let parent = stub_runtime().with_inherit_full_registry(true);
    // Simulate SubAgentToolRegistry::new setting the parent's own basis.
    let mut parent = parent;
    parent.child_subset_basis = Some(vec!["read_file".to_string()]);

    let child = parent.child_runtime();
    assert!(
        child.inherit_full_registry,
        "inherit_full_registry must propagate to children"
    );
    assert_eq!(
        child.child_subset_basis,
        Some(vec!["read_file".to_string()]),
        "child_subset_basis must propagate so the child's build_allowed_tools can intersect"
    );

    // background_runtime delegates to child_runtime, so it must propagate too.
    let bg = parent.background_runtime();
    assert!(bg.inherit_full_registry);
    assert_eq!(bg.child_subset_basis, Some(vec!["read_file".to_string()]));
}

#[test]
fn parse_spawn_request_extracts_cwd_when_present() {
    let input = json!({
        "prompt": "build feature A",
        "cwd": ".worktrees/feature-a"
    });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert_eq!(
        parsed.cwd.as_ref().map(|p| p.to_string_lossy().to_string()),
        Some(".worktrees/feature-a".to_string())
    );
}

#[test]
fn parse_spawn_request_cwd_absent_yields_none() {
    let input = json!({ "prompt": "no cwd" });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert!(parsed.cwd.is_none());
}

#[test]
fn parse_spawn_request_cwd_empty_string_yields_none() {
    let input = json!({ "prompt": "empty cwd", "cwd": "   " });
    let parsed = parse_spawn_request(&input).expect("spawn request should parse");
    assert!(parsed.cwd.is_none(), "whitespace-only cwd should be None");
}

#[test]
fn build_subagent_system_prompt_appends_role_when_set() {
    let assignment = SubAgentAssignment::new("p".to_string(), Some("worker".to_string()));
    let prompt = build_subagent_system_prompt(&SubAgentType::General, &assignment, None);
    assert!(
        prompt.ends_with("You are operating in the role of `worker`."),
        "expected role line at end, got: {}",
        &prompt[prompt.len().saturating_sub(80)..]
    );
}

#[test]
fn build_subagent_system_prompt_skips_role_when_none() {
    let assignment = SubAgentAssignment::new("p".to_string(), None);
    let prompt = build_subagent_system_prompt(&SubAgentType::General, &assignment, None);
    assert!(!prompt.contains("You are operating in the role of"));
}

#[test]
fn build_subagent_system_prompt_skips_role_when_blank() {
    let assignment = SubAgentAssignment::new("p".to_string(), Some("   ".to_string()));
    let prompt = build_subagent_system_prompt(&SubAgentType::General, &assignment, None);
    assert!(!prompt.contains("You are operating in the role of"));
}

#[test]
fn subagent_done_sentinel_format_is_well_formed() {
    let res = make_snapshot(SubAgentStatus::Completed);
    let sentinel = subagent_done_sentinel("agent_xyz", &res);
    assert!(sentinel.starts_with("<codesmith:subagent.done>"));
    assert!(sentinel.ends_with("</codesmith:subagent.done>"));

    // The inner JSON parses and carries the expected fields.
    let inner = sentinel
        .trim_start_matches("<codesmith:subagent.done>")
        .trim_end_matches("</codesmith:subagent.done>");
    let parsed: serde_json::Value = serde_json::from_str(inner).expect("inner JSON parses");
    assert_eq!(parsed["agent_id"], "agent_xyz");
    assert_eq!(parsed["status"], "completed");
    assert_eq!(parsed["agent_type"], "general");
    assert_eq!(parsed["summary_location"], "previous_line");
    assert_eq!(parsed["details"], "agent_eval");
    assert!(parsed.get("summary").is_none());
    assert!(parsed.get("duration_ms").is_none());
    assert!(parsed.get("steps").is_none());
}

#[test]
fn subagent_failed_sentinel_format_is_well_formed() {
    let sentinel = subagent_failed_sentinel("agent_zzz", "boom");
    let inner = sentinel
        .trim_start_matches("<codesmith:subagent.done>")
        .trim_end_matches("</codesmith:subagent.done>");
    let parsed: serde_json::Value = serde_json::from_str(inner).expect("inner JSON parses");
    assert_eq!(parsed["agent_id"], "agent_zzz");
    assert_eq!(parsed["status"], "failed");
    assert_eq!(parsed["error_location"], "previous_line");
    assert_eq!(parsed["details"], "agent_eval");
    assert!(parsed.get("error").is_none());
}

#[test]
fn subagent_runtime_default_max_depth_is_three() {
    // Sanity-check the constant — bumping it without a test means stale docs.
    assert_eq!(DEFAULT_MAX_SPAWN_DEPTH, 3);
}

#[test]
fn would_exceed_depth_at_boundary() {
    // depth=2, max=3 → next spawn (depth 3) is allowed (allow-equal).
    // depth=3, max=3 → next spawn (depth 4) exceeds.
    let runtime = stub_runtime();
    let mut at_max = runtime.clone();
    at_max.spawn_depth = 3;
    at_max.max_spawn_depth = 3;
    assert!(
        at_max.would_exceed_depth(),
        "depth 3 + max 3 → next would be 4, exceeds"
    );

    let mut below_max = runtime;
    below_max.spawn_depth = 2;
    below_max.max_spawn_depth = 3;
    assert!(
        !below_max.would_exceed_depth(),
        "depth 2 + max 3 → next is 3, allowed"
    );
}

#[test]
fn child_runtime_increments_depth_and_preserves_auto_approve() {
    let mut parent = stub_runtime();
    parent.spawn_depth = 1;
    parent.context.auto_approve = false; // parent in suggest mode
    let child = parent.child_runtime();
    assert_eq!(child.spawn_depth, 2, "child depth = parent + 1");
    assert_eq!(child.step_api_timeout, DEFAULT_STEP_API_TIMEOUT);
    assert!(
        !child.context.auto_approve,
        "child must inherit parent approval state"
    );
    assert!(!parent.context.auto_approve);

    parent.context.auto_approve = true;
    let auto_child = parent.child_runtime();
    assert!(
        auto_child.context.auto_approve,
        "auto-approved parents should still create auto-approved children"
    );
}

#[test]
fn child_and_background_runtimes_preserve_step_api_timeout() {
    let timeout = Duration::from_secs(7);
    let parent = stub_runtime().with_step_api_timeout(timeout);

    let child = parent.child_runtime();
    assert_eq!(child.step_api_timeout, timeout);

    let background = parent.background_runtime();
    assert_eq!(background.step_api_timeout, timeout);
}

#[tokio::test]
async fn subagent_registry_blocks_approval_tools_without_parent_auto_approve() {
    let mut runtime = stub_runtime();
    runtime.context.auto_approve = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        Some(vec!["exec_shell".to_string()]),
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    let err = registry
        .execute("agent_test", "exec_shell", json!({"command": "echo hi"}))
        .await
        .expect_err("approval-gated child tool should be blocked");

    assert!(
        err.to_string().contains("requires approval"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn implementer_delegation_allows_suggest_write_without_parent_auto_approve() {
    // Issue #1828: implementer agents could not write files even when their
    // whole job is to land code changes, because the registry blocked every
    // approval-gated tool when the parent ran in `suggest` mode. The
    // hardened gate (#1833) delegates `Suggest`-level tools (write_file,
    // edit_file, apply_patch) to write-capable roles.
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(workspace.clone());
    runtime.context.auto_approve = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Implementer,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    let result = registry
        .execute(
            "agent_test",
            "write_file",
            json!({"path": "delegated.txt", "content": "hello"}),
        )
        .await
        .expect("delegated write should be allowed for implementer");

    let written = std::fs::read_to_string(workspace.join("delegated.txt"))
        .expect("file should exist after delegated write");
    assert_eq!(written, "hello");
    assert!(
        !result.contains("requires approval"),
        "successful write should not look like an approval error: {result}"
    );
}

#[tokio::test]
async fn general_delegation_still_blocks_suggest_write_without_parent_auto_approve() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(workspace.clone());
    runtime.context.auto_approve = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    let err = registry
        .execute(
            "agent_test",
            "write_file",
            json!({"path": "general.txt", "content": "ok"}),
        )
        .await
        .expect_err("general agent should not silently gain write permission");
    let msg = err.to_string();
    assert!(
        msg.contains("not delegated to general sub-agents"),
        "general writes should be rejected with a role-aware message: {msg}"
    );

    assert!(
        !workspace.join("general.txt").exists(),
        "general write must not land without parent auto-approve"
    );
}

#[tokio::test]
async fn explore_role_still_blocks_suggest_writes_without_parent_auto_approve() {
    // Read-only stances (explore, plan, review, verifier) must not gain
    // write capabilities via delegation — otherwise a parent that asked
    // for "just look at the code" could find files mutated behind its back.
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.context.auto_approve = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Explore,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    let err = registry
        .execute(
            "agent_test",
            "write_file",
            json!({"path": "should_not_appear.txt", "content": "denied"}),
        )
        .await
        .expect_err("explore agents must not write");
    let msg = err.to_string();
    // The type's read-only surface is the first gate now (the write tool is not
    // in the allowlist at all), so the refusal names the tool rather than the
    // delegation rule.
    assert!(
        msg.contains("write_file") && msg.contains("not allowed"),
        "explore writes must be rejected by the read-only allowlist: {msg}"
    );
    assert!(
        !tmp.path().join("should_not_appear.txt").exists(),
        "file must not have been written"
    );
}

#[tokio::test]
async fn delegated_write_role_still_blocks_required_tools() {
    // Required-level tools (exec_shell, etc.) remain gated behind parent
    // auto-approve regardless of role. Implementer can write files, but it
    // still can't bypass shell approval just because it's a "write" role.
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.context.auto_approve = false;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::Implementer,
        Some(vec!["exec_shell".to_string()]),
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    let err = registry
        .execute("agent_test", "exec_shell", json!({"command": "echo hi"}))
        .await
        .expect_err("Required-level shell must still need parent auto-approve");
    assert!(
        err.to_string().contains(
            "cannot run inside this sub-agent unless the parent session is auto-approved"
        ),
        "expected Required-level approval message, got: {err}"
    );
}

#[tokio::test]
async fn auto_approved_parent_runs_required_tools_in_subagent() {
    // Baseline: when the parent runtime IS auto-approved, every approval
    // class is permitted (same as before the delegation hardening).
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.context.auto_approve = true;
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );

    // Calling exec_shell with interactive=true is what we block via the
    // separate terminal-takeover guard; pick the simpler write-file path
    // to assert that approval gating is off when auto_approve is set.
    registry
        .execute(
            "agent_test",
            "write_file",
            json!({"path": "auto.txt", "content": "auto"}),
        )
        .await
        .expect("auto-approved parent should allow writes");
}

#[test]
fn child_cancellation_cascades_from_parent() {
    let parent = stub_runtime();
    let child = parent.child_runtime();
    assert!(!child.cancel_token.is_cancelled());
    parent.cancel_token.cancel();
    assert!(
        child.cancel_token.is_cancelled(),
        "parent cancel() must propagate to child via child_token()"
    );
}

#[test]
fn mailbox_propagates_through_child_runtime_chain() {
    use crate::tools::subagent::mailbox::Mailbox;
    let parent_token = CancellationToken::new();
    let (mailbox, _rx) = Mailbox::new(parent_token.clone());

    let mut parent = stub_runtime();
    parent.cancel_token = parent_token;
    parent.mailbox = Some(mailbox);

    let child = parent.child_runtime();
    let grandchild = child.child_runtime();
    assert!(parent.mailbox.is_some());
    assert!(child.mailbox.is_some(), "child inherits parent mailbox");
    assert!(
        grandchild.mailbox.is_some(),
        "grandchild inherits via the cloned Arc inside Mailbox"
    );
}

#[test]
fn subagent_rejects_interactive_shell_terminal_takeover() {
    let err = reject_subagent_terminal_takeover(
        "exec_shell",
        &serde_json::json!({
            "command": "python3 -i",
            "interactive": true
        }),
    )
    .expect_err("sub-agents must not inherit the parent terminal");

    let msg = err.to_string();
    assert!(msg.contains("cannot use exec_shell with interactive=true"));
    assert!(msg.contains("parent terminal"));

    reject_subagent_terminal_takeover(
        "exec_shell",
        &serde_json::json!({
            "command": "cargo check",
            "interactive": false
        }),
    )
    .expect("non-interactive shell remains allowed");
    reject_subagent_terminal_takeover(
        "exec_shell",
        &serde_json::json!({
            "command": "cargo test",
            "background": true
        }),
    )
    .expect("background shell remains allowed");
}

#[tokio::test]
async fn mailbox_close_as_cancel_propagates_to_grandchild_runtime() {
    use crate::tools::subagent::mailbox::Mailbox;
    let parent_token = CancellationToken::new();
    let (mailbox, _rx) = Mailbox::new(parent_token.clone());

    let mut parent = stub_runtime();
    parent.cancel_token = parent_token;
    parent.mailbox = Some(mailbox.clone());

    let child = parent.child_runtime();
    let grandchild = child.child_runtime();
    assert!(!grandchild.cancel_token.is_cancelled());

    // Close the mailbox via *any* clone — the original or the one stored on
    // the runtime. Cancellation must reach all the way to the grandchild.
    mailbox.close();
    assert!(parent.cancel_token.is_cancelled());
    assert!(child.cancel_token.is_cancelled());
    assert!(
        grandchild.cancel_token.is_cancelled(),
        "close-as-cancel must propagate across max_spawn_depth=3"
    );
}

#[tokio::test]
async fn mailbox_orders_messages_from_parent_and_child_runtimes() {
    use crate::tools::subagent::mailbox::{Mailbox, MailboxMessage};
    let parent_token = CancellationToken::new();
    let (mailbox, mut rx) = Mailbox::new(parent_token.clone());

    let mut parent = stub_runtime();
    parent.cancel_token = parent_token;
    parent.mailbox = Some(mailbox);
    let child = parent.child_runtime();

    // Interleave sends from both runtimes; sequence numbers stay monotonic.
    parent
        .mailbox
        .as_ref()
        .unwrap()
        .send(MailboxMessage::progress("parent_a", "step 1"));
    child
        .mailbox
        .as_ref()
        .unwrap()
        .send(MailboxMessage::progress("child_b", "step 1"));
    parent
        .mailbox
        .as_ref()
        .unwrap()
        .send(MailboxMessage::progress("parent_a", "step 2"));

    let drained = rx.drain();
    assert_eq!(drained.len(), 3);
    assert_eq!(drained[0].seq, 1);
    assert_eq!(drained[1].seq, 2);
    assert_eq!(drained[2].seq, 3);
    // Verify ordering is preserved across publishers.
    match (
        &drained[0].message,
        &drained[1].message,
        &drained[2].message,
    ) {
        (
            MailboxMessage::Progress { agent_id: a, .. },
            MailboxMessage::Progress { agent_id: b, .. },
            MailboxMessage::Progress { agent_id: c, .. },
        ) => {
            assert_eq!(a, "parent_a");
            assert_eq!(b, "child_b");
            assert_eq!(c, "parent_a");
        }
        other => panic!("unexpected message order: {other:?}"),
    }
}

#[test]
fn persisted_empty_allowed_tools_loads_as_full_inheritance() {
    // Backward-compat: a v0.6.5 session that persisted with an empty Vec
    // (or a v0.6.6 session with no narrowing) should load as None on
    // restart, meaning full inheritance.
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("subagents.v1.json");
    let payload = serde_json::json!({
        "schema_version": SUBAGENT_STATE_SCHEMA_VERSION,
        "agents": [{
            "id": "agent_test",
            "agent_type": "general",
            "prompt": "p",
            "assignment": { "objective": "p" },
            "status": "Completed",
            "result": null,
            "steps_taken": 0,
            "duration_ms": 0,
            "allowed_tools": [],
            "updated_at_ms": 0
        }]
    });
    std::fs::write(&state_path, payload.to_string()).unwrap();

    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 5).with_state_path(state_path);
    manager.load_state().expect("load should succeed");
    let agent = manager.agents.get("agent_test").expect("loaded agent");
    assert!(
        agent.allowed_tools.is_none(),
        "empty Vec on disk → None (full inheritance)"
    );
}

#[test]
fn persisted_non_empty_allowed_tools_loads_as_narrow() {
    // Backward-compat the other way: a v0.6.5 session that persisted with
    // an explicit narrow list keeps that list on reload.
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("subagents.v1.json");
    let payload = serde_json::json!({
        "schema_version": SUBAGENT_STATE_SCHEMA_VERSION,
        "agents": [{
            "id": "agent_narrow",
            "agent_type": "custom",
            "prompt": "p",
            "assignment": { "objective": "p" },
            "status": "Completed",
            "result": null,
            "steps_taken": 0,
            "duration_ms": 0,
            "allowed_tools": ["read_file", "list_dir"],
            "updated_at_ms": 0
        }]
    });
    std::fs::write(&state_path, payload.to_string()).unwrap();

    let mut manager = SubAgentManager::new(dir.path().to_path_buf(), 5).with_state_path(state_path);
    manager.load_state().expect("load should succeed");
    let agent = manager.agents.get("agent_narrow").expect("loaded agent");
    assert_eq!(
        agent.allowed_tools.as_deref(),
        Some(&["read_file".to_string(), "list_dir".to_string()][..]),
        "non-empty Vec → Some(list), narrow scope preserved"
    );
}

/// Build a minimal `SubAgentRuntime` for tests that exercise pure runtime
/// helpers (depth, cancellation, child_runtime). Doesn't construct a real
/// HTTP client — calls that hit `runtime.client` would fail, but the
/// helpers we test here don't.
fn stub_runtime() -> SubAgentRuntime {
    use tokio_util::sync::CancellationToken;

    let workspace = std::env::temp_dir().join("codesmith-test-stub");
    let context = ToolContext::new(workspace.clone());
    SubAgentRuntime {
        client: stub_client(),
        model: "deepseek-v4-flash".to_string(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
        role_models: std::collections::HashMap::new(),
        auto_route: crate::config::AutoRouteContext::default(),
        context,
        allow_shell: true,
        event_tx: None,
        approval_router: None,
        extension_runner: None,
        manager: new_shared_subagent_manager(workspace, 5),
        spawn_depth: 0,
        max_spawn_depth: DEFAULT_MAX_SPAWN_DEPTH,
        cancel_token: CancellationToken::new(),
        mailbox: None,
        parent_completion_tx: None,
        fork_context: None,
        mcp_pool: None,
        step_api_timeout: DEFAULT_STEP_API_TIMEOUT,
        inherit_full_registry: false,
        children_read_only: false,
        child_subset_basis: None,
        capability_disabled_tools: std::collections::HashSet::new(),
        external_provider: None,
    }
}

/// A minimal stub client. Test helpers below only ever check struct fields
/// (depth, cancel_token, context); they don't call the network. `Config::default()`
/// is enough — `resolve_llm_client` only validates that an API key field exists,
/// not that the key works.
fn stub_client() -> crate::llm_client::LlmClientHandle {
    let config = crate::config::Config {
        api_key: Some("test-key".to_string()),
        ..crate::config::Config::default()
    };
    crate::core::engine::resolve_llm_client(&config).expect("stub client should construct")
}

// ---- #405 session-boundary classification ----
//
// Each manager assigns a fresh session_boot_id; agents stamp the id at
// spawn time. After persist + reload by a *new* manager, those agents
// carry the prior boot id and are classified as `from_prior_session`.
// `agent_list` defaults to current-session only; `include_archived=true`
// surfaces the prior-session records with the flag set.

fn insert_prior_session_agent(
    manager: &mut SubAgentManager,
    id: &str,
    status: SubAgentStatus,
    boot_id: &str,
) {
    let (input_tx, _input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        id.to_string(),
        SubAgentType::General,
        "old prompt".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        None,
        None,
        input_tx,
        boot_id.to_string(),
    );
    agent.status = status;
    agent.id = id.to_string();
    manager.agents.insert(id.to_string(), agent);
}

#[test]
fn session_boot_ids_are_unique_per_manager() {
    let a = SubAgentManager::new(PathBuf::from("."), 1);
    let b = SubAgentManager::new(PathBuf::from("."), 1);
    assert_ne!(a.session_boot_id(), b.session_boot_id());
}

#[test]
fn list_filtered_drops_prior_session_terminals_by_default() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 5);
    let current_boot = manager.session_boot_id().to_string();
    insert_prior_session_agent(
        &mut manager,
        "current_running",
        SubAgentStatus::Running,
        &current_boot,
    );
    insert_prior_session_agent(
        &mut manager,
        "prior_completed",
        SubAgentStatus::Completed,
        "boot_old_session",
    );
    insert_prior_session_agent(
        &mut manager,
        "prior_running",
        SubAgentStatus::Running,
        "boot_old_session",
    );

    let listed = manager.list_filtered(false);
    let ids: Vec<&str> = listed.iter().map(|s| s.agent_id.as_str()).collect();
    assert!(ids.contains(&"current_running"), "{ids:?}");
    assert!(
        ids.contains(&"prior_running"),
        "still-running prior-session agents stay visible: {ids:?}"
    );
    assert!(
        !ids.contains(&"prior_completed"),
        "completed prior-session agents are hidden by default: {ids:?}"
    );

    let prior = listed
        .iter()
        .find(|s| s.agent_id == "prior_running")
        .unwrap();
    assert!(prior.from_prior_session);
    let current = listed
        .iter()
        .find(|s| s.agent_id == "current_running")
        .unwrap();
    assert!(!current.from_prior_session);
}

#[test]
fn list_filtered_with_include_archived_returns_everything() {
    let mut manager = SubAgentManager::new(PathBuf::from("."), 5);
    let current_boot = manager.session_boot_id().to_string();
    insert_prior_session_agent(
        &mut manager,
        "current_done",
        SubAgentStatus::Completed,
        &current_boot,
    );
    insert_prior_session_agent(
        &mut manager,
        "prior_done",
        SubAgentStatus::Completed,
        "boot_old",
    );
    insert_prior_session_agent(
        &mut manager,
        "prior_failed",
        SubAgentStatus::Failed("boom".to_string()),
        "boot_old",
    );

    let listed = manager.list_filtered(true);
    assert_eq!(listed.len(), 3, "{listed:?}");
    let prior = listed.iter().find(|s| s.agent_id == "prior_done").unwrap();
    assert!(prior.from_prior_session);
    let current = listed
        .iter()
        .find(|s| s.agent_id == "current_done")
        .unwrap();
    assert!(!current.from_prior_session);
}

#[test]
fn agents_with_empty_boot_id_classify_as_prior_session() {
    // Records persisted before #405 land with an empty `session_boot_id`
    // due to `#[serde(default)]`. The manager treats those the same as
    // a non-matching id — i.e. prior session.
    let mut manager = SubAgentManager::new(PathBuf::from("."), 5);
    insert_prior_session_agent(&mut manager, "legacy", SubAgentStatus::Completed, "");

    let listed_default = manager.list_filtered(false);
    assert!(
        listed_default.iter().all(|s| s.agent_id != "legacy"),
        "legacy completed agents are hidden by default"
    );

    let listed_archived = manager.list_filtered(true);
    let legacy = listed_archived
        .iter()
        .find(|s| s.agent_id == "legacy")
        .unwrap();
    assert!(legacy.from_prior_session);
}

#[test]
fn persist_round_trip_preserves_session_boot_id() {
    let dir = tempdir().expect("tempdir");
    let state_path = dir.path().join(SUBAGENT_STATE_FILE);

    let original_boot;
    {
        let mut writer =
            SubAgentManager::new(dir.path().to_path_buf(), 2).with_state_path(state_path.clone());
        original_boot = writer.session_boot_id().to_string();
        insert_prior_session_agent(
            &mut writer,
            "agent_persist",
            SubAgentStatus::Completed,
            &original_boot,
        );
        writer
            .persist_state()
            .expect("persist round-trip should write");
    }

    // A fresh manager comes up with a *different* boot id and reloads
    // the persisted state; the agent should now be classified prior.
    let mut reader =
        SubAgentManager::new(dir.path().to_path_buf(), 2).with_state_path(state_path.clone());
    reader.load_state().expect("reload should succeed");
    assert_ne!(reader.session_boot_id(), original_boot);

    let listed_default = reader.list_filtered(false);
    assert!(
        !listed_default.iter().any(|s| s.agent_id == "agent_persist"),
        "completed prior-session agent hidden after reload: {listed_default:?}"
    );
    let listed_all = reader.list_filtered(true);
    let snap = listed_all
        .iter()
        .find(|s| s.agent_id == "agent_persist")
        .unwrap();
    assert!(snap.from_prior_session);
}

// === Issue #756: parent-completion wakeup ===
//
// When a direct child of the engine finishes, `run_subagent_task` emits
// a `SubAgentCompletion` on the runtime's `parent_completion_tx`. The
// engine's turn loop drains that channel before deciding to end the turn.
// These tests cover the gating logic in `emit_parent_completion` so the
// parent isn't flooded with grandchild completions and so the function
// is safe when no channel is wired.

fn runtime_with_depth(
    spawn_depth: u32,
    parent_completion_tx: Option<mpsc::UnboundedSender<SubAgentCompletion>>,
) -> SubAgentRuntime {
    let mut rt = stub_runtime();
    rt.spawn_depth = spawn_depth;
    rt.parent_completion_tx = parent_completion_tx;
    rt
}

#[test]
fn emit_parent_completion_fires_for_direct_child() {
    let (tx, mut rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    let runtime = runtime_with_depth(1, Some(tx));

    let sent = emit_parent_completion(&runtime, "agent_abc", "summary line\n<sentinel/>");

    assert!(sent, "depth=1 with channel wired should send");
    let received = rx.try_recv().expect("channel should have one message");
    assert_eq!(received.agent_id, "agent_abc");
    assert_eq!(received.payload, "summary line\n<sentinel/>");
    assert!(rx.try_recv().is_err(), "should be exactly one message");
}

#[test]
fn emit_parent_completion_skips_grandchildren() {
    let (tx, mut rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    let runtime = runtime_with_depth(2, Some(tx));

    let sent = emit_parent_completion(&runtime, "agent_grandchild", "ignored");

    assert!(
        !sent,
        "depth=2 grandchild must not fire on the parent channel"
    );
    assert!(
        rx.try_recv().is_err(),
        "channel should remain empty for grandchildren"
    );
}

#[test]
fn emit_parent_completion_skips_engine_self() {
    // depth 0 is the engine itself — the engine never spawns a task at
    // depth 0, but defend against accidental misuse.
    let (tx, mut rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    let runtime = runtime_with_depth(0, Some(tx));

    let sent = emit_parent_completion(&runtime, "agent_root", "ignored");

    assert!(
        !sent,
        "depth=0 must not fire (only depth=1 direct children)"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn emit_parent_completion_no_channel_is_noop() {
    let runtime = runtime_with_depth(1, None);

    let sent = emit_parent_completion(&runtime, "agent_no_chan", "anything");

    assert!(
        !sent,
        "missing channel should be a silent no-op, not a panic"
    );
}

#[test]
fn emit_parent_completion_dropped_receiver_does_not_panic() {
    let (tx, rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    drop(rx);
    let runtime = runtime_with_depth(1, Some(tx));

    // The send returns an error internally but we discard it — the
    // caller's run_subagent_task does not care whether the engine is
    // still listening (it might be shutting down).
    let sent = emit_parent_completion(&runtime, "agent_orphan", "after-rx-drop");

    assert!(
        sent,
        "we still attempt the send; the engine being gone is not our problem"
    );
}

#[tokio::test]
async fn run_subagent_task_emits_parent_completion_before_terminal_update() {
    let manager = Arc::new(RwLock::new(SubAgentManager::new(PathBuf::from("."), 2)));
    let (task_input_tx, task_input_rx) = mpsc::unbounded_channel();
    let agent_id = "agent_noop".to_string();
    let mut agent = SubAgent::new(
        agent_id.clone(),
        SubAgentType::General,
        "noop".to_string(),
        make_assignment(),
        "deepseek-v4-flash".to_string(),
        None,
        None,
        task_input_tx,
        "boot_test".to_string(),
    );
    agent.status = SubAgentStatus::Running;
    manager.write().await.agents.insert(agent_id.clone(), agent);

    let (completion_tx, mut completion_rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    let mut runtime = runtime_with_depth(1, Some(completion_tx));
    runtime.manager = Arc::clone(&manager);

    let task = SubAgentTask {
        manager_handle: manager.clone(),
        runtime,
        agent_id: agent_id.clone(),
        agent_type: SubAgentType::General,
        prompt: "no-op child run".to_string(),
        assignment: make_assignment(),
        allowed_tools: None,
        fork_context: false,
        agent_memory: None,
        started_at: Instant::now(),
        max_steps: 0,
        input_rx: task_input_rx,
    };

    let manager_lock = manager.write().await;
    let task_handle = tokio::spawn(run_subagent_task(task));

    // While the manager write lock is held, completion can be emitted only if it
    // is sent before the terminal-state manager update (the ordering fixed by
    // issue #1961).
    let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
        .await
        .expect("completion should be emitted while manager write lock is still held");
    let completion = completion.expect("completion channel should remain open");
    assert_eq!(completion.agent_id, agent_id);

    drop(manager_lock);
    task_handle
        .await
        .expect("run_subagent_task should complete after lock release");

    let snapshot = {
        let manager = manager.read().await;
        manager
            .get_result(&agent_id)
            .expect("completed agent should be present")
    };
    assert_eq!(snapshot.status, SubAgentStatus::Completed);
}

#[test]
fn child_runtime_propagates_completion_tx_for_gating() {
    // The channel is cloned through `child_runtime()` so descendants carry
    // it. The gate at the send site (`spawn_depth == 1`) is what limits
    // who actually fires — `child_runtime` simply must not strand it.
    let (tx, _rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
    let parent = runtime_with_depth(0, Some(tx));

    let child = parent.child_runtime();

    assert_eq!(child.spawn_depth, 1, "child increments depth");
    assert!(
        child.parent_completion_tx.is_some(),
        "child carries the wakeup channel forward"
    );
}

#[test]
fn subagent_runtime_default_step_api_timeout_is_legacy_120s() {
    // The legacy hardcoded constant is now the default field value so existing
    // call sites and tests that construct a runtime without explicit timeout
    // wiring keep their old behavior (#1806, #1808).
    let runtime = stub_runtime();
    assert_eq!(runtime.step_api_timeout, DEFAULT_STEP_API_TIMEOUT);
    assert_eq!(
        DEFAULT_STEP_API_TIMEOUT,
        std::time::Duration::from_secs(crate::config::DEFAULT_SUBAGENT_API_TIMEOUT_SECS)
    );
}

#[test]
fn with_step_api_timeout_overrides_runtime_field() {
    let runtime = stub_runtime().with_step_api_timeout(std::time::Duration::from_secs(900));
    assert_eq!(runtime.step_api_timeout.as_secs(), 900);
}

#[test]
fn child_runtime_preserves_step_api_timeout() {
    // Real sub-agents spawn through `child_runtime()` / `background_runtime()`;
    // forgetting to clone the timeout would silently drop the user's config
    // override and resurrect the 120 s default for every child step.
    let parent = stub_runtime().with_step_api_timeout(std::time::Duration::from_secs(900));
    let child = parent.child_runtime();
    let background = parent.background_runtime();

    assert_eq!(
        child.step_api_timeout.as_secs(),
        900,
        "child_runtime must preserve parent's per-step timeout"
    );
    assert_eq!(
        background.step_api_timeout.as_secs(),
        900,
        "background_runtime (detached) must also preserve the parent's timeout"
    );
}

#[test]
fn subagent_completion_payload_carries_existing_sentinel_format() {
    // The payload format is the same one already documented in
    // prompts/base.md: human summary on line 1, `<codesmith:subagent.done>`
    // sentinel on line 2. This test pins the format so future refactors
    // don't silently break the model's parsing contract.
    let mut snap = make_snapshot(SubAgentStatus::Completed);
    snap.result = Some("Found three errors.".to_string());

    let summary = summarize_subagent_result(&snap);
    let sentinel = subagent_done_sentinel("agent_test", &snap);
    let payload = format!("{summary}\n{sentinel}");

    let mut lines = payload.lines();
    let first = lines.next().expect("first line is summary");
    let second = lines.next().expect("second line is sentinel");
    assert!(
        !first.starts_with("<codesmith:subagent.done>"),
        "summary should not be the sentinel itself"
    );
    assert!(
        second.starts_with("<codesmith:subagent.done>"),
        "second line is the sentinel"
    );
    assert!(second.ends_with("</codesmith:subagent.done>"));
    assert!(
        second.contains("\"agent_id\":\"agent_test\""),
        "sentinel JSON includes agent_id"
    );
    assert!(
        !second.contains("Found three errors."),
        "sentinel should not duplicate the human summary line"
    );
}

// === Explore read-only enforcement (docs/SUBAGENTS.md: "explore — read-only") ===

#[test]
fn explore_registry_narrows_to_read_only_tools() {
    let registry = make_test_registry(SubAgentType::Explore, None);

    let allowed = registry
        .allowed_tools
        .as_ref()
        .expect("explore must carry an explicit read-only allowlist");
    assert!(
        allowed.iter().any(|name| name == "read_file"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "write_file"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "exec_shell"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "agent_open"),
        "an explore child is a leaf — it cannot spawn children: {allowed:?}"
    );
    // Every kept name is read-only per the registry's own declarations.
    for name in allowed {
        let caps = registry
            .registry
            .get(name)
            .expect("a kept name is registered")
            .capabilities();
        assert!(
            caps.contains(&ToolCapability::ReadOnly),
            "'{name}' is not read-only: {caps:?}"
        );
    }
    assert!(registry.refused_non_read_only_tools().is_empty());
}

#[test]
fn explore_spawn_refuses_requested_write_tools() {
    let registry = make_test_registry(
        SubAgentType::Explore,
        Some(vec!["read_file".to_string(), "write_file".to_string()]),
    );

    assert_eq!(
        registry.refused_non_read_only_tools(),
        ["write_file".to_string()],
        "a requested write tool is reported, not silently dropped"
    );
    assert_eq!(
        registry.allowed_tools.as_ref().expect("narrowed"),
        &vec!["read_file".to_string()]
    );
}

#[test]
fn general_registry_keeps_the_full_surface() {
    let registry = make_test_registry(SubAgentType::General, None);

    assert!(
        registry.allowed_tools.is_none(),
        "the read-only narrowing belongs to explore, not to every type"
    );
    assert!(registry.refused_non_read_only_tools().is_empty());
}

// === child cwd outside the workspace (`/trust add <path>`) ===

#[test]
fn child_cwd_needs_the_workspace_or_a_trusted_root() {
    let dir = tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    let other_repo = dir.path().join("other-repo");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::create_dir_all(&other_repo).expect("second repo dir");
    let other_repo = other_repo.canonicalize().expect("canonical second repo");
    let workspace = workspace.canonicalize().expect("canonical workspace");

    let untrusted = ToolContext::new(workspace.clone());
    let err = SubagentRunTool::validate_cwd(&other_repo, &workspace, &untrusted)
        .expect_err("an untrusted external cwd is refused");
    assert!(
        format!("{err}").contains("/trust add"),
        "the refusal must name the fix: {err}"
    );

    let trusted =
        ToolContext::new(workspace.clone()).with_trusted_external_paths(vec![other_repo.clone()]);
    let resolved = SubagentRunTool::validate_cwd(&other_repo, &workspace, &trusted)
        .expect("a trusted external cwd is allowed");
    assert_eq!(resolved, other_repo);

    // Inside the workspace stays allowed without any trust entry.
    let inner = ToolContext::new(workspace.clone());
    assert!(SubagentRunTool::validate_cwd(&workspace, &workspace, &inner).is_ok());
}

// === child path-access questions bubble to the parent UI ===

/// Build a child registry whose context is `workspace`, wired to a host's
/// event channel + approval router — the shape `spawn_subagent` produces.
fn child_registry_with_host(
    workspace: &std::path::Path,
    allowed: Option<Vec<String>>,
) -> (
    SubAgentToolRegistry,
    tokio::sync::mpsc::Sender<crate::core::engine::ApprovalDecision>,
    tokio::sync::mpsc::Receiver<crate::core::events::Event>,
) {
    let (tx_event, rx_event) = tokio::sync::mpsc::channel(8);
    let (tx_approval, rx_approval) = tokio::sync::mpsc::channel(8);
    let router = Arc::new(crate::core::engine::ApprovalRouter::new(rx_approval));

    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(workspace.to_path_buf());
    runtime.event_tx = Some(tx_event);
    runtime.approval_router = Some(router);
    let registry = SubAgentToolRegistry::new(
        runtime,
        SubAgentType::General,
        allowed,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    (registry, tx_approval, rx_event)
}

/// A child that hits the workspace boundary asks the parent's UI instead of
/// failing: the question is emitted on the parent's event channel, the answer
/// returns through the shared router, and the approval grants the path in the
/// live trust list the child shares with its parent — so the call that asked
/// runs, in the same turn.
#[tokio::test]
async fn child_path_escape_asks_the_parent_and_runs_once_approved() {
    use crate::core::engine::ApprovalDecision;
    use crate::core::events::Event;

    let workspace = tempdir().expect("workspace tempdir");
    let outside = tempdir().expect("outside tempdir");
    let file = outside.path().join("notes.md");
    std::fs::write(&file, "sibling notes").unwrap();

    let (registry, tx_approval, mut rx_event) =
        child_registry_with_host(workspace.path(), Some(vec!["read_file".to_string()]));

    let path = file.to_string_lossy().into_owned();
    let call = tokio::spawn(async move {
        registry
            .execute("agent_1", "read_file", json!({ "path": path }))
            .await
    });

    // The question reaches the host (the UI that spawned the child)…
    let id = loop {
        match rx_event.recv().await {
            Some(Event::PathAccessRequired {
                id,
                tool_name,
                path,
            }) => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(path, file.canonicalize().expect("canonical file"));
                break id;
            }
            Some(_) => continue,
            None => panic!("the child never asked about the out-of-workspace path"),
        }
    };
    assert!(
        id.starts_with("sub:agent_1:"),
        "the ask id must be child-scoped so answers route to this waiter: {id}"
    );
    // …and the host's answer routes back to it.
    tx_approval
        .send(ApprovalDecision::Approved { id })
        .await
        .expect("send approval");

    let out = call
        .await
        .expect("join the child call")
        .expect("an approved path access must run the call");
    assert!(
        out.contains("sibling notes"),
        "the child read the file it was granted: {out}"
    );
}

/// A refusal keeps the call refused and says the path was declined — it must
/// not hand the child's model the old "run `/trust add`" hint, which is what
/// the user just turned down.
#[tokio::test]
async fn child_path_escape_denial_keeps_the_call_refused() {
    use crate::core::engine::ApprovalDecision;
    use crate::core::events::Event;

    let workspace = tempdir().expect("workspace tempdir");
    let outside = tempdir().expect("outside tempdir");
    let file = outside.path().join("notes.md");
    std::fs::write(&file, "sibling notes").unwrap();

    let (registry, tx_approval, mut rx_event) =
        child_registry_with_host(workspace.path(), Some(vec!["read_file".to_string()]));

    let path = file.to_string_lossy().into_owned();
    let call = tokio::spawn(async move {
        registry
            .execute("agent_2", "read_file", json!({ "path": path }))
            .await
    });

    let id = loop {
        match rx_event.recv().await {
            Some(Event::PathAccessRequired { id, .. }) => break id,
            Some(_) => continue,
            None => panic!("the child never asked about the out-of-workspace path"),
        }
    };
    tx_approval
        .send(ApprovalDecision::Denied { id })
        .await
        .expect("send denial");

    let err = call
        .await
        .expect("join the child call")
        .expect_err("a declined path must fail the call");
    let message = err.to_string();
    assert!(message.contains("declined access"), "{message}");
    assert!(
        !message.contains("/trust add"),
        "a declined path must not be re-offered as a `/trust add` hint: {message}"
    );
}

#[test]
fn plan_mode_runtime_narrows_every_child_type_to_read_only() {
    // Plan mode sets `children_read_only`: a *general* child (write-capable by
    // type) still gets only the read-only tools of its own registry — the mode
    // cannot write or execute, so its investigators must not either.
    let registry = make_test_registry_with(SubAgentType::General, None, true);

    let allowed = registry
        .allowed_tools
        .as_ref()
        .expect("a read-only child carries an explicit allowlist");
    assert!(
        allowed.iter().any(|name| name == "read_file"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "write_file"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "edit_file"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "exec_shell"),
        "{allowed:?}"
    );
    assert!(
        !allowed.iter().any(|name| name == "agent_open"),
        "a read-only child is a leaf: {allowed:?}"
    );
    for name in allowed {
        let caps = registry
            .registry
            .get(name)
            .expect("a kept name is registered")
            .capabilities();
        assert!(
            caps.contains(&ToolCapability::ReadOnly),
            "'{name}' is not read-only: {caps:?}"
        );
    }
}

#[test]
fn plan_mode_child_asking_for_a_write_tool_fails_loudly() {
    let registry = make_test_registry_with(
        SubAgentType::General,
        Some(vec!["read_file".to_string(), "edit_file".to_string()]),
        true,
    );

    assert_eq!(
        registry.refused_non_read_only_tools(),
        ["edit_file".to_string()],
        "a read-only mode must refuse a requested write tool, not run with it dropped"
    );
}

// === session-before-fork veto ===============================================

/// A minimal `ExtensionCommandContext` for binding the test runner.
struct ForkCtx;
#[async_trait::async_trait]
impl codesmith_agent::extension::ExtensionContext for ForkCtx {
    fn cwd(&self) -> &std::path::Path {
        std::path::Path::new(".")
    }
    fn mode(&self) -> codesmith_agent::extension::ExtensionMode {
        codesmith_agent::extension::ExtensionMode::Interactive
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn signal(&self) -> tokio_util::sync::CancellationToken {
        tokio_util::sync::CancellationToken::new()
    }
    fn generation(&self) -> u64 {
        1
    }
}
impl codesmith_agent::extension::ExtensionCommandContext for ForkCtx {}

/// Records every event it sees; optionally cancels on `SessionBeforeFork`.
struct ForkHandler {
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    cancel_fork: bool,
}
#[async_trait::async_trait]
impl codesmith_agent::extension::Handler for ForkHandler {
    async fn handle(
        &self,
        event: &codesmith_agent::extension::ExtensionEvent,
        _ctx: &dyn codesmith_agent::extension::ExtensionContext,
    ) -> Result<
        codesmith_agent::extension::HandlerOutcome,
        codesmith_agent::extension::ExtensionError,
    > {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{:?}", event.kind()));
        if self.cancel_fork
            && matches!(
                event.kind(),
                codesmith_agent::extension::ExtensionEventKind::SessionBeforeFork
            )
        {
            return Ok(codesmith_agent::extension::HandlerOutcome::Cancel {
                reason: "no fork in tests".to_string(),
            });
        }
        Ok(codesmith_agent::extension::HandlerOutcome::Continue)
    }
}

struct ForkExt {
    handler: std::sync::Arc<ForkHandler>,
}
#[async_trait::async_trait]
impl codesmith_agent::extension::Extension for ForkExt {
    fn metadata(&self) -> &codesmith_agent::extension::ExtensionMetadata {
        static M: std::sync::OnceLock<codesmith_agent::extension::ExtensionMetadata> =
            std::sync::OnceLock::new();
        M.get_or_init(|| {
            codesmith_agent::extension::ExtensionMetadata::from_strings(
                "fork-veto-test".to_string(),
                "fork-veto-test".to_string(),
                "0.1.0".to_string(),
            )
        })
    }
    async fn configure(
        &self,
        api: &dyn codesmith_agent::extension::ExtensionApi,
    ) -> Result<(), codesmith_agent::extension::ExtensionError> {
        api.on(self.handler.clone())?;
        Ok(())
    }
}

async fn runner_with_fork_handler(
    cancel_fork: bool,
) -> (
    std::sync::Arc<codesmith_extensions::ExtensionRunner>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = std::sync::Arc::new(codesmith_extensions::ExtensionRunner::new());
    let ext = ForkExt {
        handler: std::sync::Arc::new(ForkHandler {
            seen: seen.clone(),
            cancel_fork,
        }),
    };
    runner.load(&ext).await.expect("configure ForkExt");
    runner.bind_core(std::sync::Arc::new(ForkCtx));
    (runner, seen)
}

#[tokio::test]
async fn session_before_fork_cancel_vetoes_agent_spawn() {
    let (runner, seen) = runner_with_fork_handler(true).await;
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let manager = Arc::new(tokio::sync::RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let tool = AgentSpawnTool::new(manager, runtime).with_extension_runner(runner.clone());

    let err = tool
        .execute(
            serde_json::json!({"prompt": "map the repo"}),
            &ToolContext::new(tmp.path().to_path_buf()),
        )
        .await
        .expect_err("Cancel must abort the spawn");
    assert!(
        err.to_string().contains("vetoed"),
        "veto error surfaced: {err}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|k| k.contains("SessionBeforeFork")),
        "the fork seam fired: {:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn session_before_fork_continue_proceeds_past_the_seam() {
    // Continue (the default): the spawn runs its normal course — on the
    // stub client it may fail later, but never with the veto error, and
    // the seam was observed.
    let (runner, seen) = runner_with_fork_handler(false).await;
    let tmp = tempdir().expect("tempdir");
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let manager = Arc::new(tokio::sync::RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let tool = AgentSpawnTool::new(manager, runtime).with_extension_runner(runner);

    let outcome = tool
        .execute(
            serde_json::json!({"prompt": "map the repo"}),
            &ToolContext::new(tmp.path().to_path_buf()),
        )
        .await;
    assert!(
        !matches!(&outcome, Err(e) if e.to_string().contains("vetoed")),
        "no veto on Continue: {outcome:?}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|k| k.contains("SessionBeforeFork")),
        "the fork seam fired: {:?}",
        seen.lock().unwrap()
    );
}

/// The veto reaches every generation: a CHILD runtime (depth 1) derived
/// through `child_runtime()` carries the runner, so the spawn tool inside
/// the child's OWN registry — the grandchild fork — is vetoable too.
#[tokio::test]
async fn session_before_fork_cancel_vetoes_grandchild_spawn() {
    let (runner, seen) = runner_with_fork_handler(true).await;
    let tmp = tempdir().expect("tempdir");
    let mut parent = stub_runtime().with_extension_runner(Some(runner));
    parent.context = ToolContext::new(tmp.path().to_path_buf());
    // `agent_spawn` is approval-Required; the child registry's gate needs
    // the inherited auto-approve to reach the tool (and its veto seam).
    parent.context.auto_approve = true;
    let child = parent.child_runtime();
    assert_eq!(child.spawn_depth, 1);
    assert!(
        child.extension_runner.is_some(),
        "the runner rides child_runtime()"
    );

    let registry = SubAgentToolRegistry::new(
        child,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let err = registry
        .execute(
            "agent-child",
            "agent_spawn",
            serde_json::json!({"prompt": "fork a grandchild"}),
        )
        .await
        .expect_err("Cancel must abort the grandchild spawn");
    assert!(
        err.to_string().contains("vetoed"),
        "veto error surfaced: {err}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|k| k.contains("SessionBeforeFork")),
        "the fork seam fired from the child's registry: {:?}",
        seen.lock().unwrap()
    );
}

/// A dylib-origin fork veto keeps its main-turn effect (depth 0: Global
/// emit — the main executor holds the `Library` alive through the turn)
/// but is skipped from a child context (depth ≥ 1), where a detached
/// background session can outlive a reload's two-phase `Library` drop:
/// the grandchild seam dispatches at the child's agent-type scope, whose
/// origin filter excludes dylib handlers (§F5d).
#[tokio::test]
async fn session_before_fork_dylib_handler_skipped_in_child_context() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = std::sync::Arc::new(codesmith_extensions::ExtensionRunner::new());
    let ext = ForkExt {
        handler: std::sync::Arc::new(ForkHandler {
            seen: seen.clone(),
            cancel_fork: true,
        }),
    };
    runner
        .load_as(&ext, codesmith_extensions::RegistrationOrigin::Dylib)
        .await
        .expect("configure ForkExt as dylib-origin");
    runner.bind_core(std::sync::Arc::new(ForkCtx));
    let tmp = tempdir().expect("tempdir");

    // Depth 0 — the main-turn contract holds: the dylib veto fires.
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    let manager = Arc::new(tokio::sync::RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        4,
    )));
    let tool = AgentSpawnTool::new(manager, runtime).with_extension_runner(runner.clone());
    let err = tool
        .execute(
            serde_json::json!({"prompt": "map the repo"}),
            &ToolContext::new(tmp.path().to_path_buf()),
        )
        .await
        .expect_err("depth-0 dylib veto must fire");
    assert!(err.to_string().contains("vetoed"), "{err}");

    // Depth 1 — the same handler from a child context: not consulted.
    seen.lock().unwrap().clear();
    let mut parent = stub_runtime().with_extension_runner(Some(runner));
    parent.context = ToolContext::new(tmp.path().to_path_buf());
    parent.context.auto_approve = true;
    let child = parent.child_runtime();
    assert_eq!(child.spawn_depth, 1);
    let registry = SubAgentToolRegistry::new(
        child,
        SubAgentType::General,
        None,
        Arc::new(Mutex::new(TodoList::new())),
        Arc::new(Mutex::new(PlanState::default())),
    );
    let outcome = registry
        .execute(
            "agent-child",
            "agent_spawn",
            serde_json::json!({"prompt": "fork a grandchild"}),
        )
        .await;
    assert!(
        !matches!(&outcome, Err(e) if e.to_string().contains("vetoed")),
        "dylib handler must not run from a child context: {outcome:?}"
    );
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|k| k.contains("SessionBeforeFork")),
        "dylib handler must not even observe the child-context seam: {:?}",
        seen.lock().unwrap()
    );
}

// === External sub-agent provider (seam consumer) ===

/// Mock provider: records every request, optionally blocks on a gate (so a
/// test can hold an agent slot open), and returns a canned outcome.
struct MockExternalProvider {
    seen: std::sync::Mutex<Vec<ExternalSubagentRequest>>,
    outcome: std::result::Result<ExternalSubagentOutcome, String>,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl MockExternalProvider {
    fn completed(output: &str, steps: u32) -> Self {
        Self {
            seen: std::sync::Mutex::new(Vec::new()),
            outcome: Ok(ExternalSubagentOutcome {
                status: SubAgentStatus::Completed,
                output: output.to_string(),
                steps_taken: steps,
                model: Some("deepseek-v4-flash".to_string()),
            }),
            gate: None,
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            seen: std::sync::Mutex::new(Vec::new()),
            outcome: Err(message.to_string()),
            gate: None,
        }
    }

    fn gated(mut self, gate: Arc<tokio::sync::Notify>) -> Self {
        self.gate = Some(gate);
        self
    }
}

#[async_trait]
impl ExternalSubagentProvider for MockExternalProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn run(
        &self,
        request: ExternalSubagentRequest,
    ) -> anyhow::Result<ExternalSubagentOutcome> {
        self.seen.lock().unwrap().push(request);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.outcome
            .clone()
            .map_err(|message| anyhow::anyhow!(message))
    }
}

/// Poll the manager until the agent reaches a terminal state (bounded).
async fn wait_until_terminal(
    manager_handle: &SharedSubAgentManager,
    agent_id: &str,
) -> SubAgentResult {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = {
            let manager = manager_handle.read().await;
            manager.get_result(agent_id).expect("agent exists")
        };
        if snapshot.status != SubAgentStatus::Running {
            return snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "agent {agent_id} never reached a terminal state"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn runtime_with_external_provider(provider: Arc<dyn ExternalSubagentProvider>) -> SubAgentRuntime {
    let mut runtime = stub_runtime().background_runtime();
    runtime.external_provider = Some(provider);
    runtime
}

#[tokio::test]
async fn external_provider_result_lands_in_manager() {
    let provider = Arc::new(MockExternalProvider::completed("external says hi", 4));
    let runtime = runtime_with_external_provider(provider.clone());
    let manager_handle = runtime.manager.clone();

    let snapshot = {
        let mut manager = manager_handle.write().await;
        manager
            .spawn_background_with_assignment(
                manager_handle.clone(),
                runtime.clone(),
                SubAgentType::Explore,
                "map the repo".to_string(),
                SubAgentAssignment::new("map the repo".to_string(), Some("scout".to_string())),
                None,
            )
            .expect("spawn should be accepted")
    };

    let result = wait_until_terminal(&manager_handle, &snapshot.agent_id).await;
    assert_eq!(result.status, SubAgentStatus::Completed);
    assert_eq!(result.result.as_deref(), Some("external says hi"));
    assert_eq!(result.steps_taken, 4);
    assert_eq!(result.context_mode, "fresh");

    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "provider must be invoked exactly once");
    let request = &seen[0];
    assert_eq!(request.agent_id, snapshot.agent_id);
    // background_runtime() of a depth-0 stub is the child's own depth.
    assert_eq!(request.spawn_depth, 1);
    assert_eq!(request.max_spawn_depth, runtime.max_spawn_depth);
    assert!(
        request.system_prompt.contains("exploration sub-agent"),
        "role instruction must ride the request"
    );
    assert!(request.prompt.contains("map the repo"));
    assert_eq!(request.workspace, runtime.context.workspace);
    assert!(!request.auto_approve, "stub context is not auto-approved");
}

#[tokio::test]
async fn external_provider_failure_marks_agent_failed() {
    let provider = Arc::new(MockExternalProvider::failing("child exploded"));
    let runtime = runtime_with_external_provider(provider.clone());
    let manager_handle = runtime.manager.clone();

    let snapshot = {
        let mut manager = manager_handle.write().await;
        manager
            .spawn_background_with_assignment(
                manager_handle.clone(),
                runtime,
                SubAgentType::General,
                "do the thing".to_string(),
                SubAgentAssignment::new("do the thing".to_string(), None),
                None,
            )
            .expect("spawn should be accepted")
    };

    let result = wait_until_terminal(&manager_handle, &snapshot.agent_id).await;
    let SubAgentStatus::Failed(error) = result.status else {
        panic!("expected failure, got {:?}", result.status);
    };
    assert!(error.contains("mock"), "{error}");
    assert!(error.contains("child exploded"), "{error}");
}

#[tokio::test]
async fn external_provider_respects_capacity_limit() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(MockExternalProvider::completed("held", 0).gated(gate.clone()));
    let mut runtime = stub_runtime();
    runtime.manager = new_shared_subagent_manager(std::env::temp_dir(), 1);
    let mut runtime = runtime.background_runtime();
    runtime.external_provider = Some(provider.clone());
    let manager_handle = runtime.manager.clone();

    let first = {
        let mut manager = manager_handle.write().await;
        manager
            .spawn_background_with_assignment(
                manager_handle.clone(),
                runtime.clone(),
                SubAgentType::General,
                "first".to_string(),
                SubAgentAssignment::new("first".to_string(), None),
                None,
            )
            .expect("first spawn fills the single slot")
    };

    let second = {
        let mut manager = manager_handle.write().await;
        manager.spawn_background_with_assignment(
            manager_handle.clone(),
            runtime.clone(),
            SubAgentType::General,
            "second".to_string(),
            SubAgentAssignment::new("second".to_string(), None),
            None,
        )
    };
    let err = second.expect_err("capacity limit must reject the second spawn");
    assert!(
        err.to_string().contains("Sub-agent limit reached"),
        "{err:#}"
    );

    gate.notify_one();
    let result = wait_until_terminal(&manager_handle, &first.agent_id).await;
    assert_eq!(result.status, SubAgentStatus::Completed);
    assert_eq!(
        provider.seen.lock().unwrap().len(),
        1,
        "only the admitted spawn may reach the provider"
    );

    let third = {
        let mut manager = manager_handle.write().await;
        manager.spawn_background_with_assignment(
            manager_handle.clone(),
            runtime,
            SubAgentType::General,
            "third".to_string(),
            SubAgentAssignment::new("third".to_string(), None),
            None,
        )
    };
    third.expect("slot freed after completion must accept a new spawn");
}

#[tokio::test]
async fn external_provider_rejects_fork_context_at_spawn() {
    let provider = Arc::new(MockExternalProvider::completed("unused", 0));
    let runtime = runtime_with_external_provider(provider.clone());
    let manager_handle = runtime.manager.clone();

    let mut manager = manager_handle.write().await;
    let err = manager
        .spawn_background_with_assignment_options(
            manager_handle.clone(),
            runtime,
            SubAgentType::General,
            "forked task".to_string(),
            SubAgentAssignment::new("forked task".to_string(), None),
            None,
            SubAgentSpawnOptions {
                fork_context: true,
                ..SubAgentSpawnOptions::default()
            },
        )
        .expect_err("fork_context must be rejected before the provider runs");
    assert!(
        err.to_string().contains("fork_context is not supported"),
        "{err:#}"
    );
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "provider must not run for a rejected spawn"
    );
}

#[test]
fn team_children_bypass_external_provider() {
    let provider = Arc::new(MockExternalProvider::completed("unused", 0));
    let runtime = runtime_with_external_provider(provider);

    assert!(external_provider_for(&runtime, &SubAgentType::General).is_some());
    assert!(external_provider_for(&runtime, &SubAgentType::Explore).is_some());
    assert!(external_provider_for(&runtime, &SubAgentType::Team).is_none());
    assert!(
        external_provider_for(&runtime, &SubAgentType::CoordinatorWorker).is_none(),
        "coordinator workers share in-process state and stay in-process"
    );
}

#[test]
fn child_runtime_propagates_external_provider() {
    let provider: Arc<dyn ExternalSubagentProvider> =
        Arc::new(MockExternalProvider::completed("unused", 0));
    let runtime = runtime_with_external_provider(provider);
    assert!(runtime.child_runtime().external_provider.is_some());
}
