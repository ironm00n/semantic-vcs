//! `svc tui`: the review surface — entity tree, events, review queue — as a view
//! over `svc --json`, optionally hosting one ACP agent run whose permission asks are answered
//! from the queue.

mod app;
mod data;
mod syntax;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event};
use svc_agent::AgentConfig;
use tokio::sync::mpsc;

pub use app::{AgentLink, App, Pane, QueueItem, ViewMode};
pub use data::{Definition, Svc};

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

/// One frame of the review UI on the checkout at `root`, as an SVG of `cols`×`rows`
/// cells with the colours the terminal would show — a screenshot that renders on GitHub
/// and stays text. `keys` is pressed first (`"o"` for the op log, `"e"` for entities).
pub fn screenshot_svg(svc_bin: PathBuf, root: PathBuf, cols: u16, rows: u16, keys: &str) -> Result<String, String> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};
    let svc = Svc::new(svc_bin, root);
    if !svc.root_exists() {
        return Err(format!("no .svc in {}", svc.root.display()));
    }
    let mut app = App::new(svc);
    app.refresh();
    app.load_events();
    for c in keys.chars() {
        app.handle_key(&Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        app.load_events();
    }
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).map_err(|e| e.to_string())?;
    terminal.draw(|frame| app.render(frame)).map_err(|e| e.to_string())?;
    let buffer = terminal.backend().buffer();
    let hex = |c: Color, fallback: &str| -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Black => "#1d1f21".into(),
            Color::Red | Color::LightRed => "#ff7b88".into(),
            Color::Green | Color::LightGreen => "#79d991".into(),
            Color::Yellow | Color::LightYellow => "#ffbd69".into(),
            Color::Blue | Color::LightBlue => "#7cb7ff".into(),
            Color::Magenta | Color::LightMagenta => "#d7a3ff".into(),
            Color::Cyan | Color::LightCyan => "#72e0b4".into(),
            Color::Gray | Color::DarkGray => "#94a0b8".into(),
            Color::White => "#edf2ff".into(),
            _ => fallback.into(),
        }
    };
    let (cw, ch) = (8.4, 17.0);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" font-family=\"ui-monospace, SFMono-Regular, Menlo, Consolas, monospace\" font-size=\"14\">\n<rect width=\"100%\" height=\"100%\" fill=\"#0b0f17\"/>\n",
        w = cols as f64 * cw + 16.0,
        h = rows as f64 * ch + 16.0
    );
    for y in 0..rows {
        // Background runs first, then the text of the row as one <text> with tspans.
        for x in 0..cols {
            let cell = &buffer[(x, y)];
            if !matches!(cell.bg, Color::Reset) {
                out.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{cw:.1}\" height=\"{ch:.1}\" fill=\"{}\"/>\n",
                    8.0 + x as f64 * cw,
                    8.0 + y as f64 * ch,
                    hex(cell.bg, "#223")
                ));
            }
        }
        out.push_str(&format!("<text x=\"8\" y=\"{:.1}\" xml:space=\"preserve\">", 8.0 + y as f64 * ch + 13.0));
        let mut run = String::new();
        let mut run_style: Option<(String, bool)> = None;
        let flush = |out: &mut String, run: &mut String, style: &Option<(String, bool)>| {
            if run.is_empty() {
                return;
            }
            let (fill, bold) = style.clone().unwrap_or(("#d8e3f8".into(), false));
            let text = run.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            out.push_str(&format!(
                "<tspan fill=\"{fill}\"{}>{text}</tspan>",
                if bold { " font-weight=\"bold\"" } else { "" }
            ));
            run.clear();
        };
        for x in 0..cols {
            let cell = &buffer[(x, y)];
            let style = (hex(cell.fg, "#d8e3f8"), cell.modifier.contains(Modifier::BOLD));
            if run_style.as_ref() != Some(&style) {
                flush(&mut out, &mut run, &run_style);
                run_style = Some(style);
            }
            run.push_str(cell.symbol());
        }
        flush(&mut out, &mut run, &run_style);
        out.push_str("</text>\n");
    }
    out.push_str("</svg>\n");
    Ok(out)
}
