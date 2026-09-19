//! `svc tui`: the review surface — entity tree, events, review queue — as a view
//! over `svc --json`, optionally hosting one ACP agent run whose permission asks are answered
//! from the queue.

mod app;
mod data;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::EventStream;
use futures::StreamExt;
use svc_agent::AgentConfig;
use tokio::sync::mpsc;

use app::{AgentLink, App};
use data::Svc;

pub struct TuiOptions {
    pub svc_bin: PathBuf,
    pub root: PathBuf,
    /// Spawn this agent and send `task` as soon as the session is up.
    pub agent: Option<(AgentConfig, String)>,
    pub wire_log: bool,
}

/// Blocks until the user quits. Installs a panic hook that restores the terminal.
pub fn run(opts: TuiOptions) -> Result<(), String> {
    let svc = Svc::new(opts.svc_bin, opts.root);
    if !svc.root_exists() {
        return Err(format!("no .svc in {}", svc.root.display()));
    }
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let terminal = ratatui::init();
    let result = rt.block_on(run_app(terminal, svc, opts.agent, opts.wire_log));
    ratatui::restore();
    result
}

async fn run_app(
    mut terminal: ratatui::DefaultTerminal,
    svc: Svc,
    agent: Option<(AgentConfig, String)>,
    wire_log: bool,
) -> Result<(), String> {
    let mut app = App::new(svc);
    app.refresh(); // before the agent can say Ready: the pre-seed needs the entity list
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let mut driver = None;
    let mut changeset_open = false;
    if let Some((config, task)) = agent {
        match app.svc.changeset_begin(&task) {
            Ok(_) => changeset_open = true,
            Err(e) => app.error = Some(format!("changeset begin: {e}")),
        }
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        driver = Some(tokio::spawn(svc_agent::run(config, ev_tx, cmd_rx, wire_log)));
        app.agent = Some(AgentLink {
            commands: cmd_tx,
            task,
            preseed: std::env::var_os("SVC_AGENT_PRESEED").is_some(),
            running: false,
            tool_titles: Default::default(),
        });
    } else {
        drop(ev_tx);
    }

    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut input = EventStream::new();
    let mut agent_open = app.agent.is_some();

    while !app.should_quit {
        tokio::select! {
            _ = tick.tick() => {
                if app.dirty {
                    app.refresh();
                }
                terminal.draw(|f| app.render(f)).map_err(|e| e.to_string())?;
            }
            maybe = input.next() => match maybe {
                Some(Ok(ev)) => app.handle_key(&ev),
                Some(Err(e)) => return Err(e.to_string()),
                None => app.should_quit = true,
            },
            ev = ev_rx.recv(), if agent_open => match ev {
                Some(ev) => app.on_agent_event(ev),
                None => agent_open = false,
            },
        }
    }
    if let Some(agent) = &app.agent {
        let _ = agent.commands.send(svc_agent::AgentCommand::Quit);
    }
    if let Some(d) = driver {
        let _ = tokio::time::timeout(Duration::from_secs(3), d).await;
    }
    if changeset_open {
        app.svc.changeset_end().map_err(|e| format!("changeset end: {e}"))?;
    }
    Ok(())
}
