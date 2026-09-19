//! Binary tests for the svc CLI. Library tests live in `svc-repo`; this file
//! is the cargo setup that actually spawns the CLI.

use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_svc"))
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/config"),
        tmp.path(),
    );
    let _ = fs::remove_dir_all(tmp.path().join(".git"));
    let _ = fs::remove_dir_all(tmp.path().join(".svc"));
    tmp
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn output(dir: &Path, args: &[&str]) -> std::process::Output {
    bin().current_dir(dir).args(args).arg("--json").output().unwrap()
}

fn json(dir: &Path, args: &[&str]) -> Value {
    let out = output(dir, args);
    assert!(
        out.status.success(),
        "svc {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "svc {} bad json ({e}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn text(dir: &Path, args: &[&str]) -> String {
    let out = bin().current_dir(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "svc {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_json(dir: &Path, args: &[&str]) -> Value {
    let out = output(dir, args);
    assert!(!out.status.success(), "svc {} should fail", args.join(" "));
    serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!(
            "svc {} bad error json ({e}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn init_status_and_list_defs_are_json() {
    let dir = fixture();
    let init = json(dir.path(), &["init"]);
    assert!(init.get("change").is_some(), "{init}");
    let status = json(dir.path(), &["status"]);
    assert!(status["entities"].as_u64().unwrap() >= 10, "{status}");
    assert_eq!(status["semantic"], 0);
    assert_eq!(status["layout"], 0);
    let defs = json(dir.path(), &["list-defs"]);
    let n = defs["definitions"].as_array().expect("definitions").len();
    assert_eq!(n, status["entities"].as_u64().unwrap() as usize);
    let err = stderr_json(dir.path(), &["init"]);
    assert!(err["error"].as_str().unwrap().contains("already exists"), "{err}");
}

#[test]
fn rename_is_one_log_event_and_rewrites_the_caller() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    let log = json(dir.path(), &["log"]);
    let ops = log.as_array().expect("log array");
    assert_eq!(ops.len(), 1, "{log}");
    assert!(ops[0]["op"].get("Rename").is_some(), "{log}");
    let rendered = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(rendered.contains("parse_config(&raw)"), "{rendered}");
    assert!(!rendered.contains("parse(&raw)"), "{rendered}");
}

#[test]
fn workspace_add_list_forget_and_update_stale() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    let agent = tempfile::tempdir().unwrap();
    let ws = agent.path().join("agent");
    json(
        dir.path(),
        &["workspace", "add", "agent", ws.to_str().unwrap()],
    );
    assert!(ws.join(".svc-workspace").is_file());
    assert!(!ws.join(".svc").exists(), "named checkout shares the store");
    let list = json(dir.path(), &["workspace", "list"]);
    let rows = list.as_array().expect("workspace list");
    assert!(rows.iter().any(|r| r["name"] == "agent"), "{list}");
    assert!(
        rows.iter().any(|r| r["name"] == "default" && r["current"] == true),
        "{list}"
    );
    let listed = text(dir.path(), &["workspace", "list"]);
    assert!(listed.contains("agent"), "{listed}");
    let stale = json(dir.path(), &["workspace", "update-stale"]);
    assert_eq!(stale["current"], true);
    assert_eq!(stale["stale"], false);
    json(dir.path(), &["workspace", "forget", "agent"]);
    let after = json(dir.path(), &["workspace", "list"]);
    assert!(
        after.as_array().unwrap().iter().all(|r| r["name"] != "agent"),
        "{after}"
    );
}

#[test]
fn init_renders_manifests_into_an_empty_checkout() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    assert!(dir.path().join("Cargo.toml").is_file());
    let replayed = json(dir.path(), &["replay"]);
    assert!(replayed["diverged_at"].is_null(), "{replayed}");
    let agent = tempfile::tempdir().unwrap();
    let ws = agent.path().join("from-store");
    json(
        dir.path(),
        &["workspace", "add", "from-store", ws.to_str().unwrap()],
    );
    let toml = std::fs::read_to_string(ws.join("Cargo.toml")).unwrap();
    assert!(toml.contains("[package]"), "{toml}");
    assert!(ws.join("src/main.rs").is_file());
}

#[test]
fn show_def_text_is_a_utf8_item() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    let shown = json(dir.path(), &["show-def", "--entity", "Config"]);
    let src = shown["canonical"].as_str().expect("canonical");
    assert!(src.contains("Config") || src.contains("struct"), "{shown}");
    assert!(shown["bytes"]["src"].is_string() || shown["bytes"]["src"].is_array());
}

#[test]
fn show_search_heads_and_describe() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    let shown = json(dir.path(), &["show", "parse"]);
    let canonical = shown["canonical"].as_str().expect("canonical");
    assert!(canonical.contains("$0"), "{canonical}");
    assert!(canonical.contains("⟨"), "{canonical}");
    let found = json(dir.path(), &["search", "load"]);
    let matches = found["matches"].as_array().expect("matches");
    assert!(matches.iter().any(|m| m["name"] == "load"), "{found}");
    let heads = json(dir.path(), &["heads"]);
    assert_eq!(heads.as_array().unwrap().len(), 1);
    assert_eq!(heads[0]["current"], true);
    json(dir.path(), &["describe", "first change"]);
    let status = json(dir.path(), &["status"]);
    let change = status["change"].as_str().unwrap();
    let evo = json(dir.path(), &["evolog", change]);
    assert!(
        evo.as_array().unwrap().iter().any(|e| e["message"] == "first change"),
        "{evo}"
    );
}

#[test]
fn new_blame_and_undo() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    let heads = json(dir.path(), &["heads"]);
    assert_eq!(heads.as_array().unwrap().len(), 2, "{heads}");
    assert!(json(dir.path(), &["log"]).as_array().unwrap().is_empty());
    json(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    let blame = json(dir.path(), &["blame", "--entity", "parse_config"]);
    assert!(
        blame.as_array().unwrap().iter().any(|e| e["op"].get("Rename").is_some()),
        "{blame}"
    );
    json(dir.path(), &["undo"]);
    let after = json(dir.path(), &["log"]);
    assert_eq!(after[0]["op"], "Undo", "{after}");
    let defs = json(dir.path(), &["list-defs"]);
    assert!(
        defs["definitions"].as_array().unwrap().iter().any(|d| d["name"] == "parse"),
        "{defs}"
    );
}

#[test]
fn add_def_delete_and_changeset() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    let begun = json(dir.path(), &["changeset", "begin", "run"]);
    assert_eq!(begun["name"], "run");
    assert_eq!(begun["open"], true);
    json(
        dir.path(),
        &[
            "add-def",
            "--ordinal",
            "9",
            "--intent",
            "refactor",
            "--definition",
            "fn helper() {}\n",
        ],
    );
    let defs = json(dir.path(), &["list-defs"]);
    assert!(
        defs["definitions"].as_array().unwrap().iter().any(|d| d["name"] == "helper"),
        "{defs}"
    );
    let status = json(dir.path(), &["changeset", "status"]);
    assert_eq!(status["name"], "run");
    assert!(!status["ops"].as_array().unwrap().is_empty(), "{status}");
    json(dir.path(), &["delete", "--entity", "helper", "--intent", "refactor"]);
    json(dir.path(), &["changeset", "end"]);
    assert!(json(dir.path(), &["changeset", "status"]).is_null());
    let defs = json(dir.path(), &["list-defs"]);
    assert!(
        defs["definitions"].as_array().unwrap().iter().all(|d| d["name"] != "helper"),
        "{defs}"
    );
}

#[cfg(unix)]
#[test]
fn list_defs_json_survives_a_closed_pipe() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    let mut child = bin()
        .current_dir(dir.path())
        .args(["list-defs", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 1];
    let _ = stdout.read(&mut buf);
    drop(stdout);
    let status = child.wait().unwrap();
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    assert!(
        !err.contains("failed printing to stdout"),
        "broken pipe panicked: {err}"
    );
    assert!(
        status.success() || status.code() == Some(141) || status.code().is_none(),
        "unexpected exit: {status:?} stderr={err}"
    );
}
