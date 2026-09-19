use std::path::Path;

use svc_agent::{AgentConfig, AgentEvent, run_one_shot};

fn config() -> AgentConfig {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_agent.mjs");
    AgentConfig::command("node", vec![script.display().to_string()], Path::new("/tmp"))
}

#[test]
fn one_shot_streams_tool_calls_and_answers_permission() {
    let mut events = Vec::new();
    let mut asks = 0;
    let reason = run_one_shot(
        config(),
        "rename read to read_file",
        |ev| events.push(format!("{:?}", ev.out())),
        |ask| {
            asks += 1;
            assert_eq!(ask.tool_call_id, "call-1");
            assert_eq!(ask.options.len(), 2);
            true
        },
        false,
    )
    .unwrap();
    assert_eq!(reason, "end_turn");
    assert_eq!(asks, 1);
    let joined = events.join("\n");
    assert!(joined.contains("Ready"), "{joined}");
    assert!(joined.contains("Thought"), "{joined}");
    assert!(joined.contains("ToolCall { id: \"call-1\", title: \"edit_def\""), "{joined}");
    assert!(joined.contains("status: Some(\"completed\")"), "{joined}");
    assert!(joined.contains("text: \"done\""), "{joined}");
    assert!(joined.contains("Log { line: \"fake agent up\""), "stderr surfaces as Log: {joined}");
    assert!(joined.contains("Closed { error: None }"), "{joined}");
}

#[test]
fn rejecting_the_ask_reaches_the_agent() {
    let mut events = Vec::new();
    let reason = run_one_shot(config(), "x", |ev| events.push(format!("{:?}", ev.out())), |_| false, false).unwrap();
    assert_eq!(reason, "end_turn");
    let joined = events.join("\n");
    assert!(joined.contains("status: Some(\"failed\")"), "{joined}");
    assert!(joined.contains("text: \"rejected\""), "{joined}");
}

#[test]
fn a_dead_agent_is_an_error_not_a_hang() {
    let cfg = AgentConfig::command("node", vec!["-e".into(), "process.exit(3)".into()], Path::new("/tmp"));
    let err = run_one_shot(cfg, "x", |_| {}, |_| true, false).unwrap_err();
    assert!(err.contains("exited") || err.contains("closed"), "{err}");
}
