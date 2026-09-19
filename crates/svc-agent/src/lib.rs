//! The ACP client side of `svc agent`: spawn `dsh --profile acp` with the svc
//! overlay, stream its `session/update`s as [`AgentEvent`]s over **one unbounded channel**,
//! and park every `session/request_permission` as a [`PermissionAsk`] the reviewer answers
//! later. Handlers hold the dispatch loop, so nothing here blocks or parses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, CancelNotification, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo, Error, Responder};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub const DSH_PACKAGE: &str = "@deepseek-ai/dsh@0.1.5-rc.2";

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Absolute; goes in `session/new`, not the child's cwd.
    pub cwd: PathBuf,
    /// Sent as `authenticate._meta.api_key` when the agent advertises an auth method
    /// (Devin's `devin acp`); from `SVC_AGENT_API_KEY` by default.
    pub api_key: Option<String>,
}

impl AgentConfig {
    /// The stock invocation: `npx -y dsh --profile acp --patch <overlay>` with `SVC_BIN` set
    /// so the plugin shells out to the right binary.
    pub fn dsh(repo_root: &Path, overlay: &Path, svc_bin: &Path) -> Self {
        let mut env = BTreeMap::new();
        env.insert("SVC_BIN".into(), svc_bin.display().to_string());
        env.insert("DSH_TELEMETRY_DISABLED".into(), "1".into());
        Self {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                DSH_PACKAGE.into(),
                "--profile".into(),
                "acp".into(),
                "--patch".into(),
                overlay.display().to_string(),
            ],
            env,
            cwd: repo_root.to_path_buf(),
            api_key: std::env::var("SVC_AGENT_API_KEY").ok(),
        }
    }

    /// A further `--patch` overlay, e.g. the runtime one that pins the plugin's absolute path.
    pub fn with_patch(mut self, overlay: &Path) -> Self {
        self.args.push("--patch".into());
        self.args.push(overlay.display().to_string());
        self
    }

    /// Any command that speaks ACP on stdio (used by the tests' fake agent).
    pub fn command(command: impl Into<PathBuf>, args: Vec<String>, cwd: &Path) -> Self {
        Self {
            command: command.into(),
            args,
            env: BTreeMap::new(),
            cwd: cwd.to_path_buf(),
            api_key: std::env::var("SVC_AGENT_API_KEY").ok(),
        }
    }
}

/// Everything the UI needs to know, in arrival order.
#[derive(Debug)]
pub enum AgentEvent {
    Ready { session_id: String },
    Message { message_id: Option<String>, text: String },
    Thought { text: String },
    ToolCall { id: String, title: String, status: String, raw_input: Option<Value> },
    ToolCallUpdate { id: String, title: Option<String>, status: Option<String>, raw_output: Option<Value> },
    /// Answer it, or the agent waits forever.
    Permission(PermissionAsk),
    Stopped { reason: String },
    /// A line of the agent's stderr, or a wire frame when `wire_log` is on.
    Log { line: String },
    /// The connection is gone; nothing else will arrive.
    Closed { error: Option<String> },
}

/// Serialisable mirror of [`AgentEvent`] for `--json` output and logs.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEventOut {
    Ready { session_id: String },
    Message { message_id: Option<String>, text: String },
    Thought { text: String },
    ToolCall { id: String, title: String, status: String, raw_input: Option<Value> },
    ToolCallUpdate { id: String, title: Option<String>, status: Option<String>, raw_output: Option<Value> },
    Permission { tool_call_id: String, options: Vec<PermissionOptionOut> },
    Stopped { reason: String },
    Log { line: String },
    Closed { error: Option<String> },
}

#[derive(Clone, Debug, Serialize)]
pub struct PermissionOptionOut {
    pub id: String,
    pub name: String,
    pub kind: String,
}

impl AgentEvent {
    pub fn out(&self) -> AgentEventOut {
        match self {
            AgentEvent::Ready { session_id } => AgentEventOut::Ready { session_id: session_id.clone() },
            AgentEvent::Message { message_id, text } => AgentEventOut::Message {
                message_id: message_id.clone(),
                text: text.clone(),
            },
            AgentEvent::Thought { text } => AgentEventOut::Thought { text: text.clone() },
            AgentEvent::ToolCall { id, title, status, raw_input } => AgentEventOut::ToolCall {
                id: id.clone(),
                title: title.clone(),
                status: status.clone(),
                raw_input: raw_input.clone(),
            },
            AgentEvent::ToolCallUpdate { id, title, status, raw_output } => AgentEventOut::ToolCallUpdate {
                id: id.clone(),
                title: title.clone(),
                status: status.clone(),
                raw_output: raw_output.clone(),
            },
            AgentEvent::Permission(ask) => AgentEventOut::Permission {
                tool_call_id: ask.tool_call_id.clone(),
                options: ask.options.clone(),
            },
            AgentEvent::Stopped { reason } => AgentEventOut::Stopped { reason: reason.clone() },
            AgentEvent::Log { line } => AgentEventOut::Log { line: line.clone() },
            AgentEvent::Closed { error } => AgentEventOut::Closed { error: error.clone() },
        }
    }
}

/// A parked `session/request_permission`. dsh sends only the `toolCallId`; join it against
/// the `ToolCall` event that preceded it for the title and arguments.
#[derive(Debug)]
pub struct PermissionAsk {
    pub tool_call_id: String,
    pub options: Vec<PermissionOptionOut>,
    responder: Responder<RequestPermissionResponse>,
}

impl PermissionAsk {
    fn pick(&self, kind: &str) -> Option<String> {
        self.options
            .iter()
            .find(|o| o.kind == kind)
            .or_else(|| self.options.iter().find(|o| o.kind.starts_with(&kind[..5])))
            .map(|o| o.id.clone())
    }

    pub fn allow(self) -> Result<(), Error> {
        match self.pick("allow_once") {
            Some(id) => self.responder.respond(selected(id)),
            None => self.responder.respond(cancelled()),
        }
    }

    pub fn reject(self) -> Result<(), Error> {
        match self.pick("reject_once") {
            Some(id) => self.responder.respond(selected(id)),
            None => self.responder.respond(cancelled()),
        }
    }

    /// Required for every pending ask when the turn is cancelled (protocol rule).
    pub fn cancel(self) -> Result<(), Error> {
        self.responder.respond(cancelled())
    }
}

fn selected(id: String) -> RequestPermissionResponse {
    RequestPermissionResponse::new(RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)))
}

fn cancelled() -> RequestPermissionResponse {
    RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
}

#[derive(Debug)]
pub enum AgentCommand {
    Prompt(String),
    /// `session/cancel`. The holder of any [`PermissionAsk`] must `cancel()` it too.
    Cancel,
    Quit,
}

fn status_str<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Drive one agent process for the lifetime of `commands`. Returns when `Quit` arrives, the
/// command channel closes, or the agent dies. Every outcome is also reported as `Closed`.
pub async fn run(
    config: AgentConfig,
    events: UnboundedSender<AgentEvent>,
    mut commands: UnboundedReceiver<AgentCommand>,
    wire_log: bool,
) -> Result<(), Error> {
    let mut agent_config = AcpAgentConfig::new(config.command.clone()).args(config.args.clone());
    for (k, v) in &config.env {
        agent_config = agent_config.env(k.clone(), v.clone());
    }
    let log_tx = events.clone();
    let agent = AcpAgent::new(agent_config).with_debug(move |line, dir| {
        let is_stderr = format!("{dir:?}").contains("Stderr");
        if is_stderr || wire_log {
            let _ = log_tx.send(AgentEvent::Log {
                line: if is_stderr { line.to_string() } else { format!("{dir:?}: {line}") },
            });
        }
    });

    let notify_tx = events.clone();
    let perm_tx = events.clone();
    let main_tx = events.clone();
    let cwd = config.cwd.clone();
    let api_key = config.api_key.clone();

    let result = agent_client_protocol::Client
        .builder()
        .name("svc")
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                if let Some(ev) = event_of(n.update) {
                    let _ = notify_tx.send(ev);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                let ask = PermissionAsk {
                    tool_call_id: req.tool_call.tool_call_id.to_string(),
                    options: req
                        .options
                        .iter()
                        .map(|o| PermissionOptionOut {
                            id: o.option_id.to_string(),
                            name: o.name.clone(),
                            kind: status_str(&o.kind),
                        })
                        .collect(),
                    responder,
                };
                let _ = perm_tx.send(AgentEvent::Permission(ask));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |cx: ConnectionTo<Agent>| async move {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            if init.protocol_version != ProtocolVersion::V1 {
                return Err(Error::internal_error().data(serde_json::json!({
                    "reason": "unsupported protocol version",
                    "version": init.protocol_version,
                })));
            }
            // Agents that advertise an auth method (Devin's `devin acp`: `devin-browser`)
            // refuse `session/new` until `authenticate`; dsh advertises none, so this is a
            // no-op there. The key rides in `_meta.api_key`, as Devin expects.
            if let (Some(key), Some(method)) = (api_key.clone(), init.auth_methods.first()) {
                let mut meta = agent_client_protocol::schema::v1::Meta::new();
                meta.insert("api_key".into(), Value::String(key));
                cx.send_request(AuthenticateRequest::new(method.id().clone()).meta(meta))
                    .block_task()
                    .await?;
            }
            let session = cx
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            let session_id = session.session_id.clone();
            let _ = main_tx.send(AgentEvent::Ready {
                session_id: session_id.to_string(),
            });
            while let Some(cmd) = commands.recv().await {
                match cmd {
                    AgentCommand::Prompt(text) => {
                        let sent = cx.send_request(PromptRequest::new(
                            session_id.clone(),
                            vec![ContentBlock::Text(TextContent::new(text))],
                        ));
                        let tx = main_tx.clone();
                        cx.spawn(async move {
                            let reason = match sent.block_task().await {
                                Ok(r) => stop_reason(&r.stop_reason),
                                Err(e) => format!("error: {e}"),
                            };
                            let _ = tx.send(AgentEvent::Stopped { reason });
                            Ok(())
                        })?;
                    }
                    AgentCommand::Cancel => {
                        cx.send_notification(CancelNotification::new(session_id.clone()))?;
                    }
                    AgentCommand::Quit => break,
                }
            }
            Ok(())
        })
        .await;

    let _ = events.send(AgentEvent::Closed {
        error: result.as_ref().err().map(|e| e.to_string()),
    });
    result
}

fn stop_reason(r: &StopReason) -> String {
    status_str(r)
}

fn event_of(update: SessionUpdate) -> Option<AgentEvent> {
    Some(match update {
        SessionUpdate::AgentMessageChunk(c) => AgentEvent::Message {
            message_id: c.message_id.as_ref().map(|m| status_str(m)),
            text: text_of(&c.content),
        },
        SessionUpdate::AgentThoughtChunk(c) => AgentEvent::Thought {
            text: text_of(&c.content),
        },
        SessionUpdate::ToolCall(t) => AgentEvent::ToolCall {
            id: t.tool_call_id.to_string(),
            title: t.title.clone(),
            status: status_str(&t.status),
            raw_input: t.raw_input.clone(),
        },
        SessionUpdate::ToolCallUpdate(u) => AgentEvent::ToolCallUpdate {
            id: u.tool_call_id.to_string(),
            title: u.fields.title.clone(),
            status: u.fields.status.as_ref().map(status_str),
            raw_output: u.fields.raw_output.clone(),
        },
        _ => return None,
    })
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `svc agent "<task>"` without a TUI: one prompt, events to `on_event`, asks answered by
/// `decide` (true = allow). Blocks on a private runtime; returns the stop reason.
pub fn run_one_shot(
    config: AgentConfig,
    task: &str,
    mut on_event: impl FnMut(&AgentEvent),
    mut decide: impl FnMut(&PermissionAsk) -> bool,
    wire_log: bool,
) -> Result<String, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let driver = tokio::spawn(run(config, ev_tx, cmd_rx, wire_log));
        let mut reason = None;
        let mut prompted = false;
        while let Some(ev) = ev_rx.recv().await {
            on_event(&ev);
            match ev {
                AgentEvent::Ready { .. } if !prompted => {
                    prompted = true;
                    let _ = cmd_tx.send(AgentCommand::Prompt(task.to_string()));
                }
                AgentEvent::Permission(ask) => {
                    let ok = decide(&ask);
                    let _ = if ok { ask.allow() } else { ask.reject() };
                }
                AgentEvent::Stopped { reason: r } => {
                    reason = Some(r);
                    let _ = cmd_tx.send(AgentCommand::Quit);
                }
                AgentEvent::Closed { error } => {
                    if let Some(e) = error {
                        return Err(e);
                    }
                    break;
                }
                _ => {}
            }
        }
        let _ = driver.await;
        reason.ok_or_else(|| "agent closed before finishing the turn".to_string())
    })
}
