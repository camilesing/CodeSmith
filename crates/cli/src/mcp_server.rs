//! MCP server implementation for exposing DeepSeek tools over stdio.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use uuid::Uuid;

use crate::config::Config;
use crate::models::{ContentBlock, Message, MessageRequest};
use crate::tools::spec::{ToolError, ToolResult};
use crate::tools::{ToolContext, ToolRegistryBuilder};

#[derive(Debug, Default, Deserialize)]
struct McpServerConfigFile {
    #[serde(default)]
    server: McpServerSection,
}

#[derive(Debug, Default, Deserialize)]
struct McpServerSection {
    expose_tools: Option<Vec<String>>,
    require_approval: Option<bool>,
}

#[derive(Debug, Clone)]
struct McpServerSettings {
    expose_tools: Vec<String>,
    require_approval: bool,
}

impl McpServerSettings {
    fn load() -> Result<Self> {
        let path = default_config_path();
        if let Some(path) = path.filter(|p| p.exists()) {
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read MCP server config: {}", path.display()))?;
            let config: McpServerConfigFile = toml::from_str(&contents).with_context(|| {
                format!("Failed to parse MCP server config: {}", path.display())
            })?;
            let expose_tools = config
                .server
                .expose_tools
                .unwrap_or_else(default_expose_tools);
            // Fail-closed: the default exposure set includes shell /
            // apply_patch / file_write, so approval is required unless the
            // operator explicitly opts out.
            let require_approval = config.server.require_approval.unwrap_or(true);
            Ok(Self {
                expose_tools,
                require_approval,
            })
        } else {
            Ok(Self {
                expose_tools: default_expose_tools(),
                require_approval: true,
            })
        }
    }
}

#[derive(Debug, Clone)]
struct ExposedTool {
    public: String,
    internal: String,
}

pub fn run_mcp_server(workspace: PathBuf) -> Result<()> {
    let settings = McpServerSettings::load()?;
    let mut server = McpServer::new(workspace, settings)?;
    server.run()
}

/// A retained conversation thread plus its recency, for bounded eviction.
struct ThreadEntry {
    messages: Vec<Message>,
    last_used: Instant,
}

/// Upper bound on retained conversation threads. Threads are in-memory
/// only and replies only grow them, so without a cap a long-running
/// server accumulates every conversation indefinitely; beyond the cap
/// the least-recently-used thread is evicted before a new conversation
/// is inserted.
const MAX_THREADS: usize = 64;

/// LLM backend for the codesmith/codesmith-reply tools, resolved lazily
/// on first use and reused afterwards: resolving per call re-read the
/// config from disk and rebuilt the HTTP client on every request, and
/// silently picked up mid-session config changes.
struct LlmBackend {
    client: crate::llm_client::LlmClientHandle,
    /// Model used when a call does not override it — the loaded config's
    /// default, not a hardcoded id.
    default_model: String,
}

fn resolve_llm_backend() -> Result<LlmBackend, RpcError> {
    let config = Config::load(None, None).map_err(|e| RpcError {
        code: -32000,
        message: format!("Failed to load config: {e}"),
    })?;
    let client = crate::core::engine::resolve_llm_client(&config).map_err(|e| RpcError {
        code: -32000,
        message: format!("Failed to create LLM client: {e}"),
    })?;
    Ok(LlmBackend {
        client,
        default_model: config.default_model(),
    })
}

struct McpServer {
    registry: crate::tools::ToolRegistry,
    exposed_tools: Vec<ExposedTool>,
    require_approval: bool,
    /// Thread-based conversation state for codesmith/codesmith-reply tools.
    /// Maps thread_id -> ordered list of messages in the conversation.
    threads: HashMap<String, ThreadEntry>,
    /// Monotonic request counter for notification correlation.
    next_notification_id: u64,
    /// See [`LlmBackend`]; resolved on the first codesmith call.
    llm: Option<LlmBackend>,
}

impl McpServer {
    fn new(workspace: PathBuf, settings: McpServerSettings) -> Result<Self> {
        let exposed_tools = build_exposed_tools(&settings.expose_tools);
        let mut internal_names: HashSet<String> = HashSet::new();
        for tool in &exposed_tools {
            internal_names.insert(tool.internal.clone());
        }

        let mut builder = ToolRegistryBuilder::new()
            .with_file_tools()
            .with_search_tools();

        if internal_names.contains("apply_patch") {
            builder = builder.with_patch_tools();
        }
        if internal_names.contains("exec_shell") {
            builder = builder.with_shell_tools();
        }

        let context = ToolContext::new(workspace);
        let registry = builder.build(context);

        Ok(Self {
            registry,
            exposed_tools,
            require_approval: settings.require_approval,
            threads: HashMap::new(),
            next_notification_id: 0,
            llm: None,
        })
    }

    fn run(&mut self) -> Result<()> {
        let runtime = Runtime::new().context("Failed to start MCP runtime")?;
        let stdin = io::stdin();
        let mut stdout = io::stdout();

        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Value>(trimmed) {
                Ok(message) => message,
                Err(_) => {
                    // Unparseable input must not vanish silently:
                    // JSON-RPC requires a -32700 Parse error response so
                    // the peer can detect the protocol violation.
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": "Parse error" }
                    });
                    let payload = serde_json::to_string(&response)?;
                    writeln!(stdout, "{payload}")?;
                    stdout.flush()?;
                    continue;
                }
            };

            if let Some(response) = self.handle_message(&runtime, message) {
                let payload = serde_json::to_string(&response)?;
                writeln!(stdout, "{payload}")?;
                stdout.flush()?;
            }
        }

        Ok(())
    }

    fn handle_message(&mut self, runtime: &Runtime, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A missing or non-string "method" is an Invalid Request per
            // JSON-RPC 2.0; respond (with null id when absent) instead of
            // letting the peer's request vanish.
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id.unwrap_or(Value::Null),
                "error": {
                    "code": -32600,
                    "message": "Invalid Request: missing or non-string method"
                }
            }));
        };

        match method {
            "initialize" => respond(id.as_ref(), initialize_response()),
            "tools/list" => respond(id.as_ref(), self.list_tools_response()),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                match self.call_tool(runtime, params, id.clone()) {
                    Ok(result) => respond(id.as_ref(), result),
                    Err(err) => respond_error(id.as_ref(), err.code, err.message),
                }
            }
            // Resources are not advertised: every resource the server once
            // listed answered -32601 when opened (there was no
            // `resources/read`). A capability and its reader land together.
            "ping" => respond(id.as_ref(), json!({})),
            "notifications/initialized" => None,
            _ => respond_error(id.as_ref(), -32601, format!("Method not found: {method}")),
        }
    }

    fn list_tools_response(&self) -> Value {
        let mut tools = Vec::new();
        let mut seen = HashSet::new();
        for entry in &self.exposed_tools {
            if !seen.insert(entry.public.clone()) {
                continue;
            }
            match entry.internal.as_str() {
                "codesmith" => {
                    tools.push(json!({
                        "name": "codesmith",
                        "description": "Send a prompt to CodeSmith and get a response. Creates a new conversation thread.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "prompt": {
                                    "type": "string",
                                    "description": "The user prompt to send to CodeSmith"
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Optional model identifier (defaults to the configured default model)"
                                }
                            },
                            "required": ["prompt"]
                        }
                    }));
                }
                "codesmith-reply" => {
                    tools.push(json!({
                        "name": "codesmith-reply",
                        "description": "Continue an existing conversation thread with CodeSmith. Requires a thread_id from a previous codesmith call.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "thread_id": {
                                    "type": "string",
                                    "description": "Thread ID from a previous codesmith call"
                                },
                                "prompt": {
                                    "type": "string",
                                    "description": "The follow-up prompt"
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Optional model override"
                                }
                            },
                            "required": ["thread_id", "prompt"]
                        }
                    }));
                }
                _ => {
                    if let Some(tool) = self.registry.get(&entry.internal) {
                        tools.push(json!({
                            "name": entry.public,
                            "description": tool.description(),
                            "inputSchema": tool.input_schema(),
                        }));
                    }
                }
            }
        }
        json!({ "tools": tools, "nextCursor": Value::Null })
    }

    fn call_tool(
        &mut self,
        runtime: &Runtime,
        params: Value,
        request_id: Option<Value>,
    ) -> Result<Value, RpcError> {
        let params = params.as_object().ok_or_else(|| RpcError {
            code: -32602,
            message: "Invalid params for tools/call".to_string(),
        })?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing tool name".to_string(),
            })?;

        // `require_approval` must not trust the client: the caller that
        // wants the tool executed is the same party that would assert
        // `approved=true`. Approval is therefore granted out-of-band, by the
        // operator launching the server with CODESMITH_MCP_APPROVED_TOOLS
        // (comma-separated exposed tool names, or `*`). Without that grant
        // every gated call is denied.
        if self.require_approval && !tool_approved_out_of_band(name) {
            return Err(RpcError {
                code: -32001,
                message: format!(
                    "Tool '{name}' is not approved for this server. Approval is granted \
                     out-of-band by the operator via CODESMITH_MCP_APPROVED_TOOLS \
                     (comma-separated tool names, or '*'); client-side approval flags \
                     are not accepted."
                ),
            });
        }

        let internal = self
            .exposed_tools
            .iter()
            .find(|tool| tool.public == name)
            .map(|tool| tool.internal.clone())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: format!("Tool not exposed: {name}"),
            })?;

        // Handle codesmith and codesmith-reply natively
        if internal == "codesmith" || internal == "codesmith-reply" {
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            return self.handle_codesmith_call(runtime, &internal, &arguments, request_id);
        }

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = runtime.block_on(self.registry.execute_full(&internal, arguments));
        Ok(tool_result_to_mcp(result))
    }

    /// Handle a `codesmith` or `codesmith-reply` tool call.
    ///
    /// Uses the resolved LLM client directly (not the full engine) to send a
    /// prompt and return the response. For `codesmith` a new thread is created;
    /// for `codesmith-reply` the caller supplies a `thread_id` to continue an
    /// existing conversation.
    fn handle_codesmith_call(
        &mut self,
        runtime: &Runtime,
        internal_name: &str,
        arguments: &Value,
        request_id: Option<Value>,
    ) -> Result<Value, RpcError> {
        let prompt = arguments
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing required argument: prompt".to_string(),
            })?;

        // Resolve config/client once and reuse across calls (see
        // [`LlmBackend`]). Runs BEFORE the model fallback reads
        // `default_model` — on the first call `self.llm` is still `None`
        // there, and a client-controlled request without an explicit
        // `model` used to panic the whole server.
        if self.llm.is_none() {
            self.llm = Some(resolve_llm_backend()?);
        }
        let model = arguments
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.llm
                    .as_ref()
                    .expect("just resolved")
                    .default_model
                    .clone()
            });

        // Resolve thread_id
        let thread_id = if internal_name == "codesmith" {
            // New thread
            Uuid::new_v4().to_string()
        } else {
            arguments
                .get("thread_id")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "Missing required argument: thread_id for codesmith-reply".to_string(),
                })?
                .to_string()
        };

        let client = self
            .llm
            .as_ref()
            .expect("backend resolved above")
            .client
            .clone();

        // Build message list
        let user_message = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
                cache_control: None,
            }],
        };

        let messages = if internal_name == "codesmith" {
            vec![user_message]
        } else {
            let thread = self.threads.get_mut(&thread_id).ok_or_else(|| RpcError {
                code: -32602,
                message: format!("Thread not found: {thread_id}"),
            })?;
            thread.last_used = Instant::now();
            let mut existing = thread.messages.clone();
            existing.push(user_message);
            existing
        };

        // Send the API request (non-streaming for the basic version)
        let request = MessageRequest {
            model,
            messages: messages.clone(),
            max_tokens: 16384,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        };

        // Bound the upstream call: this server handles requests strictly
        // sequentially, so one hung request (dead connection with no
        // HTTP-level timeout) used to block every subsequent MCP request
        // forever with no recovery. A stuck call now fails cleanly and
        // the loop continues.
        let response = runtime
            .block_on(tokio::time::timeout(
                std::time::Duration::from_secs(300),
                client.create_message(request),
            ))
            .map_err(|_| RpcError {
                code: -32000,
                message: "DeepSeek API call timed out after 300 s".to_string(),
            })?
            .map_err(|e| RpcError {
                code: -32000,
                message: format!("DeepSeek API call failed: {e}"),
            })?;

        // Extract response text from content blocks
        let response_text = response
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text, .. } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        let usage = &response.usage;

        // Store the assistant response in the thread
        {
            // Bound memory: evict least-recently-used threads beyond the
            // cap before inserting a new conversation.
            if internal_name == "codesmith" && self.threads.len() >= MAX_THREADS {
                evict_lru_thread(&mut self.threads);
            }
            let convo = self
                .threads
                .entry(thread_id.clone())
                .or_insert(ThreadEntry {
                    messages: Vec::new(),
                    last_used: Instant::now(),
                });
            convo.last_used = Instant::now();
            // The user turn is persisted for BOTH paths — for
            // `codesmith-reply` the cloned request messages got it but the
            // stored thread did not, so later replies on the same thread
            // replayed two consecutive assistant messages and a lost user
            // turn.
            convo.messages.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: prompt.to_string(),
                    cache_control: None,
                }],
            });
            convo.messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: response_text.clone(),
                    cache_control: None,
                }],
            });
        }

        // Emit a notification/message so the client can correlate the response
        let notification_id = {
            let nid = self.next_notification_id;
            self.next_notification_id += 1;
            nid
        };

        // Write notification to stdout
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "notificationId": notification_id,
                "requestId": request_id,
                "threadId": thread_id,
                "content": response_text,
                "usage": {
                    "inputTokens": usage.input_tokens,
                    "outputTokens": usage.output_tokens,
                }
            }
        });
        if let Ok(payload) = serde_json::to_string(&notification) {
            let mut stdout = io::stdout();
            // stdout is the protocol stream: a dropped notification must
            // not be silent, so surface write/flush failures on stderr
            // instead of discarding them.
            if let Err(err) = writeln!(stdout, "{payload}").and_then(|_| stdout.flush()) {
                eprintln!("failed to write MCP notification: {err}");
            }
        }

        Ok(json!({
            "content": [{ "type": "text", "text": &response_text }],
            "isError": false,
            "structuredContent": {
                "threadId": thread_id,
                "content": response_text,
                "usage": {
                    "inputTokens": usage.input_tokens,
                    "outputTokens": usage.output_tokens,
                }
            }
        }))
    }
}

fn default_config_path() -> Option<PathBuf> {
    codesmith_config::codesmith_home()
        .ok()
        .map(|home| home.join("mcp_server.toml"))
}

/// Drop the least-recently-used thread to make room under
/// [`MAX_THREADS`].
fn evict_lru_thread(threads: &mut HashMap<String, ThreadEntry>) {
    let Some(lru_id) = threads
        .iter()
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(id, _)| id.clone())
    else {
        return;
    };
    threads.remove(&lru_id);
}

/// Whether `name` (an exposed/public tool name) was approved out-of-band by
/// the operator through the `CODESMITH_MCP_APPROVED_TOOLS` launch-time
/// environment variable. Absent or empty denies everything.
fn tool_approved_out_of_band(name: &str) -> bool {
    let Ok(granted) = std::env::var("CODESMITH_MCP_APPROVED_TOOLS") else {
        return false;
    };
    let granted = granted.trim();
    granted == "*"
        || granted
            .split(',')
            .any(|entry| entry.trim().eq_ignore_ascii_case(name))
}

fn default_expose_tools() -> Vec<String> {
    vec![
        "file_read".to_string(),
        "file_write".to_string(),
        "search".to_string(),
        "apply_patch".to_string(),
        "shell".to_string(),
        "codesmith".to_string(),
        "codesmith-reply".to_string(),
    ]
}

fn build_exposed_tools(names: &[String]) -> Vec<ExposedTool> {
    let mut tools = Vec::new();
    for name in names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let public = trimmed.to_string();
        let internal = match trimmed {
            "file_read" => "read_file",
            "file_write" => "write_file",
            "file_edit" => "edit_file",
            "shell" => "exec_shell",
            "search" => "grep_files",
            "file_search" => "file_search",
            // codesmith and codesmith-reply are handled natively in call_tool
            "codesmith" | "codesmith-reply" => trimmed,
            other => other,
        }
        .to_string();
        tools.push(ExposedTool { public, internal });
    }
    tools
}

fn tool_result_to_mcp(result: Result<ToolResult, ToolError>) -> Value {
    match result {
        Ok(tool_result) => {
            let mut response = json!({
                "content": [{ "type": "text", "text": tool_result.content }],
                "isError": !tool_result.success,
            });
            if let Some(metadata) = tool_result.metadata {
                response["structuredContent"] = metadata;
            }
            response
        }
        Err(err) => json!({
            "content": [{ "type": "text", "text": err.to_string() }],
            "isError": true,
        }),
    }
}

fn initialize_response() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": {
            "name": "codesmith-mcp-server",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": {},
        }
    })
}

fn respond(id: Option<&Value>, result: Value) -> Option<Value> {
    id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn respond_error(id: Option<&Value>, code: i64, message: String) -> Option<Value> {
    id.map(|id| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
    })
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::test_support::{EnvVarGuard, lock_test_env};
    use std::collections::HashMap;

    fn approval_server(require: bool) -> (McpServer, Runtime, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = McpServer::new(
            dir.path().to_path_buf(),
            McpServerSettings {
                expose_tools: default_expose_tools(),
                require_approval: require,
            },
        )
        .expect("server");
        let runtime = Runtime::new().expect("runtime");
        (server, runtime, dir)
    }

    #[test]
    fn client_asserted_approval_flag_is_not_trusted() {
        let _guard = lock_test_env();
        let _removed = EnvVarGuard::remove("CODESMITH_MCP_APPROVED_TOOLS");
        let (mut server, runtime, _dir) = approval_server(true);

        let err = server
            .call_tool(
                &runtime,
                json!({
                    "name": "shell",
                    "arguments": { "command": "true" },
                    "approved": true
                }),
                None,
            )
            .expect_err("client-asserted approval must be denied");
        assert_eq!(err.code, -32001);
        assert!(err.message.contains("not approved"), "got: {}", err.message);
    }

    #[test]
    fn env_allowlist_grants_out_of_band_approval() {
        let _guard = lock_test_env();
        let _granted = EnvVarGuard::set("CODESMITH_MCP_APPROVED_TOOLS", "file_read");
        let (mut server, runtime, dir) = approval_server(true);
        std::fs::write(dir.path().join("note.txt"), "hi").expect("write file");

        let result = server
            .call_tool(
                &runtime,
                json!({
                    "name": "file_read",
                    "arguments": { "path": "note.txt" }
                }),
                None,
            )
            .expect("out-of-band approved tool must execute");
        assert_eq!(result["isError"], json!(false));
    }

    #[test]
    fn message_without_method_gets_invalid_request_error() {
        let (mut server, runtime, _dir) = approval_server(false);
        let response = server
            .handle_message(&runtime, json!({ "id": 7 }))
            .expect("an invalid request must produce a response, not silence");
        assert_eq!(response["id"], json!(7));
        assert_eq!(response["error"]["code"], json!(-32600));
    }

    #[test]
    fn message_without_id_and_method_gets_null_id_error() {
        let (mut server, runtime, _dir) = approval_server(false);
        let response = server
            .handle_message(&runtime, json!({ "params": {} }))
            .expect("an invalid request must produce a response, not silence");
        assert_eq!(response["id"], json!(null));
        assert_eq!(response["error"]["code"], json!(-32600));
    }

    /// `McpServerSettings::load` with no config file under
    /// `CODESMITH_HOME`: the default exposure set and the fail-closed
    /// approval default.
    #[test]
    fn settings_load_defaults_without_a_config_file() {
        let _guard = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("CODESMITH_HOME", dir.path());

        let settings = McpServerSettings::load().expect("defaults");

        assert_eq!(settings.expose_tools, default_expose_tools());
        assert!(
            settings.require_approval,
            "the default exposure includes shell, so approval is required"
        );
    }

    #[test]
    fn settings_load_reads_the_server_section_and_fails_loud_on_bad_toml() {
        let _guard = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("CODESMITH_HOME", dir.path());
        std::fs::write(
            dir.path().join("mcp_server.toml"),
            "[server]\nexpose_tools = [\"file_read\"]\nrequire_approval = false\n",
        )
        .expect("write config");

        let settings = McpServerSettings::load().expect("settings");
        assert_eq!(settings.expose_tools, vec!["file_read".to_string()]);
        assert!(!settings.require_approval, "explicit opt-out is honored");

        std::fs::write(dir.path().join("mcp_server.toml"), "not [ a toml").expect("clobber");
        let err = McpServerSettings::load().expect_err("unparseable config must fail load");
        assert!(err.to_string().contains("Failed to parse"), "got: {err:#}");
    }

    #[test]
    fn handle_message_answers_initialize_ping_and_unknown_methods() {
        let (mut server, runtime, _dir) = approval_server(false);

        let init = server
            .handle_message(
                &runtime,
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            )
            .expect("initialize answers");
        assert_eq!(init["id"], json!(1));
        assert_eq!(init["result"]["serverInfo"]["name"], "codesmith-mcp-server");

        let ping = server
            .handle_message(
                &runtime,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
            )
            .expect("ping answers");
        assert_eq!(ping["result"], json!({}));

        let unknown = server
            .handle_message(
                &runtime,
                json!({ "jsonrpc": "2.0", "id": 3, "method": "no/such" }),
            )
            .expect("unknown methods get a JSON-RPC error, not silence");
        assert_eq!(unknown["error"]["code"], json!(-32601));
        assert!(
            unknown["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no/such")
        );

        // A notification (no id) gets no response at all.
        assert!(
            server
                .handle_message(
                    &runtime,
                    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
                )
                .is_none()
        );
        // A request-shaped message without an id is a notification too.
        assert!(
            server
                .handle_message(&runtime, json!({ "jsonrpc": "2.0", "method": "ping" }))
                .is_none()
        );
        // An error for a notification is not written either.
        assert!(
            server
                .handle_message(&runtime, json!({ "jsonrpc": "2.0", "method": "no/such" }))
                .is_none()
        );
    }

    #[test]
    fn list_tools_names_public_aliases_and_skips_unregistered_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = McpServer::new(
            dir.path().to_path_buf(),
            McpServerSettings {
                expose_tools: vec![
                    "file_read".to_string(),
                    "file_read".to_string(), // duplicates collapse
                    "bogus".to_string(),     // no registry entry → skipped
                    "codesmith".to_string(),
                    "codesmith-reply".to_string(),
                ],
                require_approval: false,
            },
        )
        .expect("server");

        let listed = server.list_tools_response();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["file_read", "codesmith", "codesmith-reply"]);
        // The `codesmith` schema names only what its handler reads: a
        // `cwd` property used to promise working-directory context that was
        // dropped on the floor.
        let codesmith = listed["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool["name"] == "codesmith")
            .expect("codesmith is listed");
        let mut properties: Vec<&str> = codesmith["inputSchema"]["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        properties.sort_unstable();
        assert_eq!(properties, ["model", "prompt"]);
        // The native tools describe their thread contract.
        assert!(
            listed["tools"]
                .to_string()
                .contains("Creates a new conversation thread")
        );
    }

    /// Found in the review: the server advertised `resources` and listed
    /// the workspace root and every session, but had no `resources/read` —
    /// each listed resource answered -32601 when a client opened it.
    #[test]
    fn resources_are_not_advertised() {
        let (mut server, runtime, _dir) = approval_server(false);

        let init = server
            .handle_message(
                &runtime,
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            )
            .expect("initialize answers");
        assert!(
            init["result"]["capabilities"].get("resources").is_none(),
            "resources are not a capability: {init}"
        );

        let listed = server
            .handle_message(
                &runtime,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }),
            )
            .expect("a request always gets a response");
        assert_eq!(listed["error"]["code"], json!(-32601), "{listed}");
    }

    #[test]
    fn call_tool_rejects_malformed_params_before_any_tool_runs() {
        let (mut server, runtime, _dir) = approval_server(false);

        let err = server
            .call_tool(&runtime, json!("not an object"), None)
            .expect_err("params must be an object");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("Invalid params"));

        let err = server
            .call_tool(&runtime, json!({ "arguments": {} }), None)
            .expect_err("name is required");
        assert!(err.message.contains("Missing tool name"));

        let err = server
            .call_tool(&runtime, json!({ "name": "grep_files" }), None)
            .expect_err("only exposed names run");
        assert!(
            err.message.contains("Tool not exposed: grep_files"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn tool_result_to_mcp_maps_metadata_and_errors() {
        let ok = tool_result_to_mcp(Ok(ToolResult {
            metadata: Some(json!({ "exit_code": 0 })),
            ..ToolResult::success("done")
        }));
        assert_eq!(ok["isError"], json!(false));
        assert_eq!(ok["content"][0]["text"], "done");
        assert_eq!(ok["structuredContent"], json!({ "exit_code": 0 }));

        let plain = tool_result_to_mcp(Ok(ToolResult::success("done")));
        assert!(plain.get("structuredContent").is_none());

        let failed = tool_result_to_mcp(Ok(ToolResult::error("boom")));
        assert_eq!(failed["isError"], json!(true));
        assert_eq!(failed["content"][0]["text"], "boom");

        let errored = tool_result_to_mcp(Err(ToolError::execution_failed(
            "dispatch blew up".to_string(),
        )));
        assert_eq!(errored["isError"], json!(true));
        assert!(
            errored["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("dispatch blew up")
        );
    }

    #[test]
    fn evict_lru_thread_drops_the_least_recently_used() {
        let mut threads = HashMap::new();
        for (id, age_secs) in [("old", 60u64), ("mid", 30), ("new", 1)] {
            threads.insert(
                id.to_string(),
                ThreadEntry {
                    messages: Vec::new(),
                    last_used: Instant::now() - std::time::Duration::from_secs(age_secs),
                },
            );
        }

        evict_lru_thread(&mut threads);
        assert!(!threads.contains_key("old"), "the LRU entry is evicted");
        assert_eq!(threads.len(), 2);

        // An empty map is a no-op, and an evicted id cannot be re-evicted.
        let mut empty: HashMap<String, ThreadEntry> = HashMap::new();
        evict_lru_thread(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn exposed_tools_map_aliases() {
        let names = vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "search".to_string(),
            "apply_patch".to_string(),
            "shell".to_string(),
        ];
        let tools = build_exposed_tools(&names);
        let mut map = HashMap::new();
        for tool in tools {
            map.insert(tool.public, tool.internal);
        }
        assert_eq!(map.get("file_read").map(String::as_str), Some("read_file"));
        assert_eq!(
            map.get("file_write").map(String::as_str),
            Some("write_file")
        );
        assert_eq!(map.get("search").map(String::as_str), Some("grep_files"));
        assert_eq!(
            map.get("apply_patch").map(String::as_str),
            Some("apply_patch")
        );
        assert_eq!(map.get("shell").map(String::as_str), Some("exec_shell"));
    }
}
