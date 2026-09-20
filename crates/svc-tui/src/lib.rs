//! `svc tui`: the review surface — entity tree, events, review queue — as a view
//! over `svc --json`, optionally hosting one ACP agent run whose permission asks are answered
//! from the queue.

mod app;
mod data;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event};
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

enum Wake {
    Input(Event),
    Tick,
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

/// Crossterm's `EventStream` shares a lock with a thread that `poll`s stdin forever.
/// `tokio::select` then polls that stream on every tick, so the runtime thread blocks
/// on the same lock — the TUI paints once and never reads keys. A dedicated poll
/// thread owns stdin; the async loop only receives.
fn spawn_wakes() -> mpsc::UnboundedReceiver<Wake> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if tx.send(Wake::Input(ev)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {
                    if tx.send(Wake::Tick).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn draw_if(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<(), String> {
    if app.need_draw {
        app.need_draw = false;
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn run_app(
    mut terminal: ratatui::DefaultTerminal,
    svc: Svc,
    agent: Option<(AgentConfig, String)>,
    wire_log: bool,
) -> Result<(), String> {
    let mut app = App::new(svc);
    // Paint before any `svc` spawn so a slow list-defs cannot look like a hung
    // alternate screen.
    terminal
        .draw(|f| app.render(f))
        .map_err(|e| e.to_string())?;
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
        driver = Some(tokio::spawn(svc_agent::run(
            config, ev_tx, cmd_rx, wire_log,
        )));
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

    terminal
        .draw(|f| app.render(f))
        .map_err(|e| e.to_string())?;
    app.need_draw = false;

    let mut wakes = spawn_wakes();
    let mut agent_open = app.agent.is_some();

    while !app.should_quit {
        tokio::select! {
            wake = wakes.recv() => match wake {
                Some(Wake::Input(ev)) => {
                    app.handle_key(&ev);
                    app.pump();
                    draw_if(&mut terminal, &mut app)?;
                }
                Some(Wake::Tick) => {
                    app.pump();
                    draw_if(&mut terminal, &mut app)?;
                }
                None => app.should_quit = true,
            },
            ev = ev_rx.recv(), if agent_open => match ev {
                Some(ev) => {
                    app.on_agent_event(ev);
                    app.pump();
                    draw_if(&mut terminal, &mut app)?;
                }
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
        app.svc
            .changeset_end()
            .map_err(|e| format!("changeset end: {e}"))?;
    }
    Ok(())
}
