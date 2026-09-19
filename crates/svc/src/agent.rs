use std::path::{Path, PathBuf};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo, LineDirection};
use svc_core::Intent;
use svc_repo::{Repo, changeset_begin, changeset_end};

/// The plugin path as written in `harness/overlay.yml`; substituted with this checkout's at run time.
const PLUGIN_PLACEHOLDER: &str = "/home/hacker/hackmit2026/harness/svc-tools.mjs";

pub async fn run(task: &str) -> Result<(), String> {
    if !has_model_credentials() {
        return Err("no model credential found; set OPENROUTER_API_KEY, DEEPSEEK_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY, or XAI_API_KEY".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = Repo::find_root(&cwd).ok_or_else(|| "not inside an svc repository".to_string())?;
    open_agent_changeset(&root)?;

    let result = run_connection(task, &root).await;
    let close_result = close_agent_changeset(&root);
    result.and(close_result)
}

fn has_model_credentials() -> bool {
    const KEYS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
    ];
    if KEYS.iter().any(|key| std::env::var_os(key).is_some()) {
        return true;
    }
    let dsh_home = std::env::var_os("DSH_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dsh"))
    });
    dsh_home.is_some_and(|home| home.join(".credentials.yaml").is_file())
}

async fn run_connection(task: &str, root: &Path) -> Result<(), String> {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let overlay = assets.join("harness/overlay.yml");
    let plugin = assets.join("harness/svc-tools.mjs");
    if !overlay.is_file() || !plugin.is_file() {
        return Err("harness assets are missing beside the source checkout".into());
    }
    // A later `--patch` replaces a row's `config`, never its `name`, so the inserted plugin
    // row must carry the absolute path from the start: rewrite the whole overlay.
    let runtime_overlay = root.join(".svc/dsh-overlay.yml");
    let text = std::fs::read_to_string(&overlay)
        .map_err(|e| e.to_string())?
        .replace(PLUGIN_PLACEHOLDER, &plugin.display().to_string());
    std::fs::write(&runtime_overlay, text).map_err(|e| e.to_string())?;
    let svc_bin = std::env::current_exe().map_err(|e| e.to_string())?;

    let config = AcpAgentConfig::new("npx")
        .args([
            "-y",
            "@deepseek-ai/dsh@0.1.5-rc.2",
            "--profile",
            "acp",
            "--patch",
            runtime_overlay.to_str().ok_or("non-UTF-8 overlay path")?,
        ])
        .env("SVC_BIN", svc_bin.to_string_lossy());
    let transport = AcpAgent::new(config).with_debug(|line, direction| {
        if direction == LineDirection::Stderr {
            eprintln!("dsh: {line}");
        }
    });

    agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                match serde_json::to_string(&notification.update) {
                    Ok(json) => println!("{json}"),
                    Err(error) => eprintln!("svc: could not encode ACP update: {error}"),
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let outcome = request.options.first().map_or(
                    RequestPermissionOutcome::Cancelled,
                    |option| {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option.option_id.clone(),
                        ))
                    },
                );
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = connection
                .send_request(NewSessionRequest::new(root.to_path_buf()))
                .block_task()
                .await?;
            connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new(task))],
                ))
                .block_task()
                .await?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())
}

fn open_agent_changeset(root: &Path) -> Result<(), String> {
    let repo = Repo::open(root, Repo::default_langs()).map_err(|e| e.to_string())?;
    changeset_begin(
        &repo,
        "agent run",
        Intent::Refactor,
        Some(std::process::id()),
        false,
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn close_agent_changeset(root: &Path) -> Result<(), String> {
    let repo = Repo::open(root, Repo::default_langs()).map_err(|e| e.to_string())?;
    changeset_end(&repo).map(|_| ()).map_err(|e| e.to_string())
}
