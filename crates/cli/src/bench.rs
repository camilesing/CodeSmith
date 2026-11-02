//! `codesmith bench` — three user-path performance benchmarks with dry-run
//! budgets (P1-3).
//!
//! Paths (one benchmark each, deliberately not the full DSH eight):
//!
//! 1. **session-open** — `FileSessionStore::load_session` over a long
//!    (600-message) saved session. Migration-shape note: the session
//!    format's adjacent migration chain is currently empty (v1 is the only
//!    on-disk version), so there is no older-version fixture to load yet;
//!    the moment P0-2 adds a v2, the fixture writer here gains the
//!    previous-version knob.
//! 2. **turn-first-token** — engine turn assembly (system prompt + full
//!    tool catalog + 600-message history journal fold) up to the first
//!    streamed assistant delta, with an immediate in-process LLM client, so
//!    the number isolates CodeSmith's own assembly cost from provider
//!    latency.
//! 3. **session-search-warm** — `search_sessions` on the P1-5 persistent
//!    FTS index at steady state (index already built; the cold build is
//!    recorded informationally as `session-search-cold`).
//!
//! Budgets are **source constants** — deliberately not env-overridable
//! (same discipline as DSH's benchmark gates): a budget someone can dial
//! out from under CI is not a budget. Enforcement is **dry-run** for now:
//! breaches are recorded in the report and printed, never non-zero-exit —
//! turning them on requires the explicit approval step recorded in
//! `docs/RELEASE_RUNBOOK.md`. The initial values are loose placeholders
//! from the first local baselines; the runbook owns the tightening.
//!
//! Everything runs under a scoped `CODESMITH_HOME` inside the output
//! directory — the benchmark never touches the user's real home. Run as
//! `cargo run -p codesmith-cli --all-features --locked -- bench` (CI uses
//! the debug profile and records `profile` in the report; comparisons are
//! only valid within one profile).
//!
//! # Known limitations
//!
//! - Wall-clock timings on a shared runner: the repeatability gate
//!   ([`REPEATABILITY_CV_MAX`]) flags noisy samples rather than failing
//!   them (dry-run), and cross-machine comparison is meaningless — trends
//!   within one CI lane are the signal.
//! - The turn-first-token fixture history lives in the engine's in-memory
//!   transcript (preloaded via `Op::SyncSession`), so the number does not
//!   include session-file load — that is `session-open`'s job.
//! - Debug-profile numbers are for regression trends only; absolute
//!   budgets tighten against release-profile baselines at promotion time.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::json;

use codesmith_agent::llm_client::LlmClient;
use codesmith_agent::models::{
    ContentBlockStart, Delta, MessageDelta as MessageDeltaFrame, MessageRequest, MessageResponse,
    StreamEvent, Usage,
};
use codesmith_agent_runtime::events::Event;
use codesmith_agent_runtime::ops::Op;

use crate::models::{ContentBlock, Message};

// === Dry-run budgets (P1-3) =============================================
// Source constants; no env override. Dry-run: breaches are recorded, not
// enforced — promotion is the runbook's explicit-approval step.

/// `session-open`: load one 600-message saved session from disk.
/// (First local baseline: ~7 ms mean, debug, mac arm64.)
pub(crate) const SESSION_OPEN_BUDGET_MS: f64 = 50.0;
/// `turn-first-token`: turn assembly up to the first assistant delta
/// (immediate in-process client — CodeSmith's own cost only).
/// (First local baseline: ~72 ms mean, debug, mac arm64.)
pub(crate) const TURN_FIRST_TOKEN_BUDGET_MS: f64 = 300.0;
/// `session-search-warm`: `search_sessions` against the persistent index.
/// (First local baseline: ~2 ms mean, debug, mac arm64.)
pub(crate) const SEARCH_WARM_BUDGET_MS: f64 = 100.0;
/// Repeatability: sample-set coefficient of variation above this flags the
/// benchmark as noisy (recorded; not a failure while dry-run).
pub(crate) const REPEATABILITY_CV_MAX: f64 = 0.35;

/// Timed samples per path. Fixed, not a flag: comparability across runs.
const SAMPLES: usize = 5;
/// Fixture sizes: long-session history and the search corpus.
const HISTORY_MESSAGES: usize = 600;
const SEARCH_SESSIONS: usize = 40;
const SEARCH_MESSAGES_PER_SESSION: usize = 30;

#[derive(clap::Args, Debug, Clone)]
pub(crate) struct BenchArgs {
    /// Output directory for `report.json` (created; also holds the scoped
    /// CODESMITH_HOME fixture tree).
    #[arg(long, value_name = "DIR", default_value = ".codesmith-bench")]
    pub out: PathBuf,
}

pub(crate) async fn run(args: BenchArgs) -> Result<()> {
    let out = args.out;
    std::fs::create_dir_all(&out).context("creating bench output dir")?;
    // Scope every home-resolved path into the output dir: the benchmark
    // must never read or write the user's real ~/.codesmith. SAFETY: this
    // runs before any thread that reads the environment is spawned.
    let home = out.join("home");
    std::fs::create_dir_all(&home).context("creating scoped home")?;
    unsafe {
        std::env::set_var("CODESMITH_HOME", &home);
    }
    run_all(&out).await
}

async fn run_all(out: &Path) -> Result<()> {
    let sessions_dir = out.join("home").join("sessions");
    std::fs::create_dir_all(&sessions_dir).context("sessions dir")?;
    let workspace = out.join("ws");
    std::fs::create_dir_all(&workspace).context("workspace dir")?;

    // Fixtures.
    let long_session = fixture_saved_session("bench-long", &workspace, HISTORY_MESSAGES);
    {
        let store = codesmith_session::store::FileSessionStore::new(sessions_dir.clone())
            .context("fixture session store")?;
        store
            .save_session(&long_session)
            .context("saving long-session fixture")?;
    }
    write_search_corpus(&sessions_dir, &workspace);

    // 1. session-open.
    let open_samples = time_samples(|| {
        let store = codesmith_session::store::FileSessionStore::new(sessions_dir.clone())
            .context("session store")?;
        let loaded = store
            .load_session("bench-long")
            .context("loading long session")?;
        anyhow::Ok(loaded.messages.len())
    });

    // 2. turn-first-token (and informational cold search below).
    let first_token_samples =
        bench_turn_first_token(&workspace, long_session.messages.clone()).await?;

    // 3. session-search: cold (index build) once, then warm samples.
    use codesmith_agent_runtime::tools::spec::ToolSpec as _;
    let tool = crate::tools::session_search::SessionSearchTool;
    let ctx = codesmith_agent_runtime::tools::spec::ToolContext::new(workspace.clone());
    let query = json!({"query": "flaky cache test", "max_results": 5});
    let t0 = Instant::now();
    let cold = tool
        .execute(query.clone(), &ctx)
        .await
        .context("cold search")?;
    let search_cold_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    let _ = cold;
    let mut warm_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = Instant::now();
        let _ = tool
            .execute(query.clone(), &ctx)
            .await
            .context("warm search")?;
        warm_samples.push(t0.elapsed().as_secs_f64() * 1_000.0);
    }

    let entries = vec![
        path_entry("session_open", &open_samples, SESSION_OPEN_BUDGET_MS),
        path_entry(
            "turn_first_token",
            &first_token_samples,
            TURN_FIRST_TOKEN_BUDGET_MS,
        ),
        path_entry("session_search_warm", &warm_samples, SEARCH_WARM_BUDGET_MS),
        informational_entry("session_search_cold_ms", search_cold_ms),
    ];

    let report = json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "host": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        "samples": SAMPLES,
        "repeatability_cv_max": REPEATABILITY_CV_MAX,
        "enforcement": "dry-run (breaches recorded, not enforced — see docs/RELEASE_RUNBOOK.md)",
        "fixtures": {
            "history_messages": HISTORY_MESSAGES,
            "search_sessions": SEARCH_SESSIONS,
            "search_messages_per_session": SEARCH_MESSAGES_PER_SESSION,
        },
        "entries": entries,
    });
    let report_path = out.join("report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report)?)
        .context("writing report.json")?;

    println!("codesmith bench — report: {}", report_path.display());
    for entry in report["entries"].as_array().context("entries")? {
        let name = entry["path"].as_str().unwrap_or("?");
        if entry.get("budget_ms").is_some() {
            println!(
                "  {name:<22} mean {:>9.2} ms  (budget {} ms, cv {}, over_budget {})",
                entry["mean_ms"].as_f64().unwrap_or(f64::NAN),
                entry["budget_ms"],
                entry["cv"],
                entry["over_budget"]
            );
        } else {
            println!(
                "  {name:<22} {:>9.2} ms  (informational)",
                entry["value_ms"].as_f64().unwrap_or(f64::NAN),
            );
        }
    }
    println!("enforcement is dry-run: over-budget entries do not fail the run");
    Ok(())
}

/// An immediate, in-process `LlmClient`: one text turn per call, no I/O —
/// so the first-token benchmark times CodeSmith's own assembly, never a
/// provider.
#[derive(Debug)]
struct ImmediateLlmClient;

impl LlmClient for ImmediateLlmClient {
    fn provider_name(&self) -> &'static str {
        "bench-immediate"
    }
    fn model(&self) -> &str {
        "bench-model"
    }
    fn create_message(
        &self,
        _request: MessageRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<MessageResponse>> + Send + '_>,
    > {
        Box::pin(async { Ok(bench_message_response()) })
    }
    fn create_message_stream(
        &self,
        _request: MessageRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = anyhow::Result<codesmith_agent::llm_client::StreamEventBox>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            let stream = async_stream::stream! {
                yield Ok(StreamEvent::MessageStart {
                    message: bench_message_response(),
                });
                yield Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: ContentBlockStart::Text {
                        text: String::new(),
                    },
                });
                yield Ok(StreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: Delta::TextDelta {
                        text: "bench first token".to_string(),
                    },
                });
                yield Ok(StreamEvent::ContentBlockStop { index: 0 });
                yield Ok(StreamEvent::MessageDelta {
                    delta: MessageDeltaFrame {
                        stop_reason: Some("end_turn".to_string()),
                        stop_sequence: None,
                    },
                    usage: Some(Usage::default()),
                });
                yield Ok(StreamEvent::MessageStop);
            };
            Ok(Box::pin(stream) as codesmith_agent::llm_client::StreamEventBox)
        })
    }
}

fn bench_message_response() -> MessageResponse {
    MessageResponse {
        id: "bench_msg".to_string(),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![],
        model: "bench-model".to_string(),
        stop_reason: None,
        stop_sequence: None,
        container: None,
        usage: Usage::default(),
    }
}

/// Time the full turn pipeline — `Op::SendMessage` on a history-preloaded
/// engine until the first streamed assistant delta, then drain to
/// `TurnComplete`. One sample per send; a fresh engine per sample so each
/// measures cold assembly (the regression that matters). Round 0 is a
/// discarded warm-up: it pays the process-wide lazy initialization (prompt
/// assembly caches, first tool setup) so the recorded samples are
/// comparable.
async fn bench_turn_first_token(workspace: &Path, history: Vec<Message>) -> Result<Vec<f64>> {
    use crate::config::Config;
    use crate::core::engine::{Engine, EngineConstruct};
    use crate::settings::Settings;

    let mut samples = Vec::with_capacity(SAMPLES);
    for round in 0..=SAMPLES {
        let config = Config::default();
        let engine_config = crate::core::engine::headless_engine_config(
            &config,
            &Settings::default(),
            crate::core::engine::HeadlessEngineOptions {
                model: "bench-model".to_string(),
                workspace: workspace.to_path_buf(),
                auto_approve: true,
                trust_mode: true,
                max_subagents: 1,
                override_system_prompt: None,
                append_system_prompts: Vec::new(),
                cache_breaker: None,
                allowed_tools: None,
                team_context: None,
            },
        )?;
        let (engine, handle) =
            <Engine as EngineConstruct>::new_with_client(engine_config, &config, {
                let client: crate::llm_client::LlmClientHandle =
                    std::sync::Arc::new(ImmediateLlmClient);
                client
            });
        let runner = tokio::spawn(async move {
            engine.run().await;
        });

        // Preload the long history, then settle (SessionUpdated ack).
        handle
            .send(Op::SyncSession {
                session_id: Some("bench-long".to_string()),
                messages: history.clone(),
                system_prompt: None,
                system_prompt_override: false,
                model: "bench-model".to_string(),
                workspace: workspace.to_path_buf(),
            })
            .await
            .context("sync session op")?;
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(15), rx.recv())
                .await
                .context("waiting for sync ack")?
                .context("engine event channel closed")?;
            if matches!(event, Event::SessionUpdated { .. }) {
                break;
            }
        }

        let t0 = Instant::now();
        let mut first_token: Option<f64> = None;
        handle
            .send(Op::SendMessage {
                content: "continue".to_string(),
                image_paths: Vec::new(),
                mode: codesmith_agent_runtime::mode::AppMode::Agent,
                model: "bench-model".to_string(),
                goal_objective: None,
                reasoning_effort: None,
                reasoning_effort_auto: false,
                auto_model: false,
                allow_shell: false,
                trust_mode: true,
                auto_approve: true,
                approval_mode: codesmith_agent_runtime::mode::ApprovalMode::Auto,
                translation_enabled: false,
                show_thinking: false,
                is_simple: false,
                allowed_tools: None,
                blocked_tools: Vec::new(),
            })
            .await
            .context("send op")?;
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv())
                .await
                .context("waiting for turn events")?
                .context("engine event channel closed")?;
            match event {
                Event::MessageDelta { .. } if first_token.is_none() => {
                    first_token = Some(t0.elapsed().as_secs_f64() * 1_000.0);
                }
                Event::TurnComplete { .. } => break,
                _ => {}
            }
        }
        drop(rx);
        let _ = handle.send(Op::Shutdown).await;
        let _ = runner.await;
        if round > 0 {
            samples.push(first_token.context("turn ended without a text delta")?);
        }
    }
    Ok(samples)
}

fn fixture_saved_session(
    id: &str,
    workspace: &Path,
    messages: usize,
) -> codesmith_session::store::SavedSession {
    let history: Vec<Message> = (0..messages)
        .map(|i| {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            Message {
                role: role.to_string(),
                content: vec![ContentBlock::Text {
                    text: format!(
                        "message {i}: we pinned the clock and dropped shared state in the \
                         flaky cache test harness, then re-ran the suite twice ({i})"
                    ),
                    cache_control: None,
                }],
            }
        })
        .collect();
    codesmith_session::store::create_saved_session_with_id_and_mode(
        id.to_string(),
        &history,
        "bench-model",
        workspace,
        0,
        None,
        None,
    )
}

/// The search corpus: mostly filler sessions, a few carrying the needle.
fn write_search_corpus(sessions_dir: &Path, workspace: &Path) {
    let store = codesmith_session::store::FileSessionStore::new(sessions_dir.to_path_buf())
        .expect("corpus session store");
    for i in 0..SEARCH_SESSIONS {
        let messages: Vec<Message> = (0..SEARCH_MESSAGES_PER_SESSION)
            .map(|m| {
                let text = if i % 10 == 0 && m == 5 {
                    format!("session {i}: the flaky cache test was fixed by pinning the clock {m}")
                } else {
                    format!("session {i} filler chatter number {m} about the weather")
                };
                Message {
                    role: (if m % 2 == 0 { "user" } else { "assistant" }).to_string(),
                    content: vec![ContentBlock::Text {
                        text,
                        cache_control: None,
                    }],
                }
            })
            .collect();
        let session = codesmith_session::store::create_saved_session_with_id_and_mode(
            format!("bench-search-{i:03}"),
            &messages,
            "bench-model",
            workspace,
            0,
            None,
            None,
        );
        store.save_session(&session).expect("save corpus session");
    }
}

fn time_samples<F>(mut f: F) -> Vec<f64>
where
    F: FnMut() -> Result<usize>,
{
    let mut samples = Vec::with_capacity(SAMPLES);
    // One discarded warm-up: first touch pays dir metadata and page cache.
    let _ = f();
    for _ in 0..SAMPLES {
        let t0 = Instant::now();
        let _proof = f().expect("bench path must keep working");
        samples.push(t0.elapsed().as_secs_f64() * 1_000.0);
    }
    samples
}

fn stats(samples: &[f64]) -> (f64, f64, f64) {
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
    let variance =
        samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / samples.len() as f64;
    (mean, min, (variance.sqrt() / mean.max(f64::EPSILON)))
}

fn path_entry(name: &str, samples: &[f64], budget_ms: f64) -> serde_json::Value {
    let (mean, min, cv) = stats(samples);
    json!({
        "path": name,
        "unit": "ms",
        "samples_ms": samples.iter().map(|s| (s * 100.0).round() / 100.0).collect::<Vec<_>>(),
        "mean_ms": (mean * 100.0).round() / 100.0,
        "min_ms": (min * 100.0).round() / 100.0,
        "cv": (cv * 1_000.0).round() / 1_000.0,
        "noisy": cv > REPEATABILITY_CV_MAX,
        "budget_ms": budget_ms,
        "over_budget": mean > budget_ms,
        "enforcement": "dry-run",
    })
}

fn informational_entry(name: &str, value_ms: f64) -> serde_json::Value {
    json!({
        "path": name,
        "unit": "ms",
        "value_ms": (value_ms * 100.0).round() / 100.0,
        "note": "informational — no budget",
    })
}
