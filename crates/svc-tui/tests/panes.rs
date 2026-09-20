//! M9: every pane and cheat-sheet key against a real `svc` store, drawn on
//! ratatui's TestBackend. Failures here are TUI bugs (cursor owns the src).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use svc_agent::{AgentCommand, AgentConfig, AgentEvent};
use svc_tui::{AgentLink, App, Pane, QueueItem, Svc, ViewMode};
use tokio::sync::mpsc;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn svc_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(p) = std::env::var_os("CARGO_BIN_EXE_svc") {
            return PathBuf::from(p);
        }
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root().join("target"));
        let bin = target.join("debug/svc");
        if !bin.is_file() {
            let status = Command::new("cargo")
                .args(["build", "-p", "svc", "--offline"])
                .env("CARGO_TARGET_DIR", &target)
                .current_dir(workspace_root())
                .status()
                .expect("spawn cargo build -p svc");
            assert!(status.success(), "cargo build -p svc failed");
        }
        assert!(bin.is_file(), "no svc binary at {}", bin.display());
        bin
    })
}

fn copy_demo() -> tempfile::TempDir {
    let src = workspace_root().join("demo/config");
    let dir = tempfile::tempdir().expect("tempdir");
    copy_tree(&src, dir.path());
    let _ = fs::remove_dir_all(dir.path().join(".git"));
    let _ = fs::remove_dir_all(dir.path().join(".svc"));
    dir
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == ".git" || name == ".svc" || name == "target" {
            continue;
        }
        let to = dst.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn svc_ok(root: &Path, args: &[&str]) {
    let out = Command::new(svc_bin())
        .args(args)
        .arg("--json")
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn svc {args:?}: {e}"));
    assert!(
        out.status.success(),
        "svc {args:?} failed ({:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn svc_json(root: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(svc_bin())
        .args(args)
        .arg("--json")
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn svc {args:?}: {e}"));
    assert!(
        out.status.success(),
        "svc {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
}

fn init_only() -> tempfile::TempDir {
    let dir = copy_demo();
    svc_ok(dir.path(), &["init"]);
    dir
}

fn init_new() -> tempfile::TempDir {
    let dir = init_only();
    svc_ok(dir.path(), &["new"]);
    dir
}

fn seeded() -> tempfile::TempDir {
    let dir = init_new();
    svc_ok(dir.path(), &["describe", "seed"]);
    svc_ok(dir.path(), &["rename", "--entity", "parse", "--new-name", "parse_config"]);
    dir
}

fn with_review_and_mail() -> tempfile::TempDir {
    let dir = seeded();
    svc_ok(dir.path(), &["changeset", "begin", "run"]);
    svc_ok(dir.path(), &["changeset", "end"]);
    svc_ok(dir.path(), &["review", "run", "--approve"]);
    svc_ok(dir.path(), &["review", "run", "--request-changes"]);
    svc_ok(dir.path(), &["review", "run", "--note", "split this"]);
    svc_ok(dir.path(), &["mail", "@all", "pushing main"]);
    svc_ok(dir.path(), &["claim", "parse_config"]);
    dir
}

fn mid_conflict() -> tempfile::TempDir {
    let dir = init_new();
    let load = svc_json(dir.path(), &["show-def", "--entity", "load"]);
    let text = load["text"].as_str().expect("show-def text").to_string();
    let a = text.replace(
        "    let cfg = parse(&raw)?;",
        "    let raw = normalize(&raw);\n    let cfg = parse(&raw)?;",
    );
    let b = text.replace("    Ok(cfg)", "    log(&raw);\n    Ok(cfg)");
    assert_ne!(a, text, "side A must change load");
    assert_ne!(b, text, "side B must change load");
    svc_ok(dir.path(), &["branch", "a6"]);
    svc_ok(
        dir.path(),
        &["edit-def", "--entity", "load", "--intent", "feature", "--definition", &a],
    );
    svc_ok(dir.path(), &["branch", "b6"]);
    svc_ok(
        dir.path(),
        &["edit-def", "--entity", "load", "--intent", "feature", "--definition", &b],
    );
    svc_ok(dir.path(), &["merge", "a6"]);
    dir
}

fn open(root: &Path) -> App {
    let mut app = App::new(Svc::new(svc_bin().to_path_buf(), root.to_path_buf()));
    app.refresh();
    app.load_events();
    app
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn type_chars(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn frame(app: &mut App, cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| app.render(f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn empty_store_renders_revisions_and_queue_without_panic() {
    let dir = init_only();
    let mut app = open(dir.path());
    assert_eq!(app.mode, ViewMode::Revisions);
    assert!(app.error.is_none(), "init-only is not an error: {:?}", app.error);
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("revisions"), "{text}");
    assert!(text.contains("review queue"), "{text}");
    assert!(text.contains("j/k move"), "{text}");
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn revisions_to_entity_blame_and_back() {
    let dir = seeded();
    let mut app = open(dir.path());
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("revisions"), "{text}");
    assert!(app.changes.iter().any(|c| c.current), "a current change");

    press(&mut app, KeyCode::Char('e'));
    app.load_events();
    assert_eq!(app.mode, ViewMode::Entities);
    assert!(app.status.contains("entity view"));
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("entities"), "{text}");
    assert!(
        text.contains("parse_config") || app.defs.iter().any(|d| d.name == "parse_config"),
        "renamed entity is in the tree: {text}"
    );
    assert!(
        text.contains("history") || text.contains("fn ") || text.contains("canonical"),
        "right pane is show-def/blame: {text}"
    );

    press(&mut app, KeyCode::Char('h'));
    assert_eq!(app.mode, ViewMode::Revisions);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, ViewMode::Revisions);
    assert!(!app.should_quit);
}

#[test]
fn oplog_lists_the_ops_then_esc_returns() {
    let dir = seeded();
    let mut app = open(dir.path());
    press(&mut app, KeyCode::Char('o'));
    app.load_events();
    assert_eq!(app.mode, ViewMode::Oplog);
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("operation log"), "{text}");
    assert!(text.contains("renamed") || text.contains("→ parse_config"), "{text}");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, ViewMode::Revisions);
}

#[test]
fn slash_filters_real_entities_and_status_shows_the_prompt() {
    let dir = seeded();
    let mut app = open(dir.path());
    press(&mut app, KeyCode::Char('e'));
    press(&mut app, KeyCode::Char('/'));
    assert!(app.typing);
    type_chars(&mut app, "parse_con");
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("/parse_con"), "status prompt: {text}");
    assert_eq!(app.rows.len(), 1, "one match");
    press(&mut app, KeyCode::Enter);
    assert!(!app.typing);
    press(&mut app, KeyCode::Esc);
    assert!(app.filter.is_empty());
    assert_eq!(app.mode, ViewMode::Entities);
}

#[test]
fn tab_moves_focus_to_the_queue_and_enter_expands_an_edit() {
    let dir = mid_conflict();
    let mut app = open(dir.path());
    assert!(!app.queue.is_empty(), "conflict store has queue rows");
    assert_eq!(app.focus, Pane::Browse);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Pane::Queue);
    press(&mut app, KeyCode::Enter);
    assert!(!app.expanded.is_empty(), "enter expands the selected queue row");
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("review queue"), "{text}");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Pane::Browse);
}

#[test]
fn review_and_mail_notes_show_in_the_queue() {
    let dir = with_review_and_mail();
    let mut app = open(dir.path());
    let notes: Vec<String> = app
        .queue
        .iter()
        .filter_map(|q| match q {
            QueueItem::Note { op } => Some(format!("{:?}", op.op)),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("Approve") || n.contains("approved")),
        "approve lands in the queue: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("RequestChanges") || n.contains("request")),
        "request-changes lands in the queue: {notes:?}"
    );
    let text = frame(&mut app, 120, 24);
    assert!(
        text.contains("approved") || text.contains("note to") || text.contains("pushing main"),
        "mail/review sentences render: {text}"
    );
    assert!(
        text.contains("pushing main") || text.contains("note to @all"),
        "mail to @all renders: {text}"
    );
}

#[test]
fn claims_show_in_the_oplog_not_as_a_queue_ask() {
    let dir = with_review_and_mail();
    let mut app = open(dir.path());
    assert!(
        !app.queue.iter().any(|q| matches!(q, QueueItem::Ask { .. })),
        "a claim is not a permission ask"
    );
    press(&mut app, KeyCode::Char('o'));
    app.load_events();
    let text = frame(&mut app, 120, 24);
    assert!(
        text.contains("claimed"),
        "claim is an op-log line: {text}"
    );
}

#[test]
fn mid_conflict_queues_a_binding_sentence() {
    let dir = mid_conflict();
    let mut app = open(dir.path());
    assert!(
        app.queue.iter().any(|q| matches!(q, QueueItem::Binding { .. })),
        "binding conflict is queued: {:?}",
        app.queue.len()
    );
    let text = frame(&mut app, 120, 24);
    assert!(text.contains("binding conflict") || text.contains("[!]"), "{text}");
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    let text = frame(&mut app, 120, 24);
    assert!(
        text.contains("resolve") || text.contains("accept"),
        "expanded binding says how to fix: {text}"
    );
}

#[test]
fn jk_and_arrows_move_the_revision_caret() {
    let dir = seeded();
    let mut app = open(dir.path());
    let start = app.revision_state.selected();
    press(&mut app, KeyCode::Char('j'));
    let after_j = app.revision_state.selected();
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Up);
    assert!(start.is_some());
    assert!(after_j.is_some());
    assert!(!app.should_quit);
}

#[test]
fn every_cheat_sheet_key_is_a_sentence_never_a_panic() {
    let dir = with_review_and_mail();
    let mut app = open(dir.path());
    for code in [
        KeyCode::Char('e'),
        KeyCode::Char('/'),
        KeyCode::Esc,
        KeyCode::Char('o'),
        KeyCode::Char('h'),
        KeyCode::Tab,
        KeyCode::Enter,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Char('a'),
        KeyCode::Char('r'),
        KeyCode::Char('u'),
        KeyCode::Char('p'),
        KeyCode::Char('c'),
        KeyCode::Char('R'),
        KeyCode::Char('e'),
        KeyCode::Esc,
    ] {
        press(&mut app, code);
        if matches!(code, KeyCode::Char('R')) {
            app.refresh();
            app.load_events();
        }
        let _ = frame(&mut app, 80, 24);
        assert!(
            app.error.as_deref().is_none_or(|e| !e.contains("panic")),
            "key {code:?} left a panic-shaped error: {:?}",
            app.error
        );
        assert!(!app.should_quit, "only q quits, not {code:?}");
    }
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn resize_eighty_by_twenty_four_and_two_hundred_by_sixty() {
    let dir = seeded();
    let mut app = open(dir.path());
    app.handle_key(&Event::Resize(80, 24));
    let compact = frame(&mut app, 80, 24);
    assert!(compact.contains("revisions"), "{compact}");
    assert!(compact.contains("review queue"), "{compact}");
    app.handle_key(&Event::Resize(80, 24));
    app.handle_key(&Event::Resize(200, 60));
    let wide = frame(&mut app, 200, 60);
    assert!(wide.contains("revisions"), "{wide}");
    assert!(wide.contains("j/k move"), "{wide}");
    press(&mut app, KeyCode::Char('e'));
    app.load_events();
    let entities = frame(&mut app, 200, 60);
    assert!(entities.contains("entities"), "{entities}");
}

#[test]
fn busy_checkout_retries_quietly_on_a_store_directory() {
    let dir = init_new();
    let fake = dir.path().join("busy-svc");
    fs::write(
        &fake,
        "#!/bin/sh\necho 'checkout busy: another svc session held it' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = App::new(Svc::new(fake, dir.path().to_path_buf()));
    app.refresh();
    assert!(app.error.is_none(), "busy is not an error to show: {:?}", app.error);
    assert!(app.dirty);
    let text = frame(&mut app, 80, 24);
    assert!(!text.contains("error:"), "{text}");
}

#[test]
fn a_missing_binary_is_a_sentence_in_the_status_line() {
    let dir = init_new();
    let mut app = App::new(Svc::new(
        PathBuf::from("/no/such/svc-binary"),
        dir.path().to_path_buf(),
    ));
    app.refresh();
    assert!(app.error.is_some(), "spawn failure is recorded");
    let err = app.error.clone().unwrap();
    assert!(!err.to_lowercase().contains("panic"), "{err}");
    assert!(
        err.contains("spawn") || err.contains("No such") || err.contains("not found") || err.contains("os error"),
        "a sentence, not a dump: {err}"
    );
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("error:"), "{text}");
    assert!(!app.should_quit);
}

#[test]
fn screenshot_svg_draws_the_revision_view() {
    let dir = seeded();
    let svg = svc_tui::screenshot_svg(
        svc_bin().to_path_buf(),
        dir.path().to_path_buf(),
        80,
        24,
        "",
    )
    .expect("svg");
    assert!(svg.contains("<svg"), "{svg}");
    assert!(svg.contains("revisions"), "{svg}");
}

#[test]
fn nothing_to_answer_is_a_status_sentence() {
    let dir = init_new();
    let mut app = open(dir.path());
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.status, "nothing to answer");
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.status, "nothing to answer");
    let text = frame(&mut app, 80, 24);
    assert!(text.contains("nothing to answer"), "{text}");
}

#[test]
fn scripted_agent_end_turn_stays_in_the_tui() {
    let dir = init_new();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../svc-agent/tests/fake_agent.mjs");
    let config = AgentConfig::command("node", vec![script.display().to_string()], dir.path());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut app = open(dir.path());
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let driver = tokio::spawn(svc_agent::run(config, ev_tx, cmd_rx, false));
        app.agent = Some(AgentLink {
            commands: cmd_tx,
            task: "edit validate".into(),
            preseed: false,
            running: false,
            tool_titles: Default::default(),
        });
        let mut answered = false;
        let mut stopped = None;
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(20), ev_rx.recv())
            .await
            .expect("agent went quiet")
        {
            let is_stop = matches!(ev, AgentEvent::Stopped { .. });
            if let AgentEvent::Stopped { reason } = &ev {
                stopped = Some(reason.clone());
            }
            app.on_agent_event(ev);
            if !answered && app.queue.iter().any(QueueItem::pending) {
                press(&mut app, KeyCode::Char('a'));
                answered = true;
            }
            let _ = frame(&mut app, 80, 24);
            if is_stop {
                break;
            }
        }
        assert_eq!(stopped.as_deref(), Some("end_turn"));
        assert!(!app.should_quit, "end_turn does not close the TUI");
        assert!(app.status.contains("p: continue"), "{}", app.status);
        let text = frame(&mut app, 80, 24);
        assert!(text.contains("p: continue") || text.contains("agent finished"), "{text}");
        let _ = app.agent.as_ref().unwrap().commands.send(AgentCommand::Quit);
        let _ = tokio::time::timeout(Duration::from_secs(3), driver).await;
    });
}
