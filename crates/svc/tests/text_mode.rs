//! Human (non `--json`) output for verbs that used to dump pretty JSON.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn is_pretty_json(s: &str) -> bool {
    s.trim_start().starts_with('{') || s.trim_start().starts_with('[')
}

#[test]
fn init_list_show_and_workspace_add_are_sentences() {
    let dir = fixture();
    let init = text(dir.path(), &["init"]);
    assert!(init.contains("initialized"), "{init}");
    assert!(init.contains("entities"), "{init}");
    assert!(!is_pretty_json(&init), "{init}");

    let listed = text(dir.path(), &["list-defs"]);
    assert!(listed.contains("parse"), "{listed}");
    assert!(!is_pretty_json(&listed), "{listed}");

    let shown = text(dir.path(), &["show-def", "--entity", "parse"]);
    assert!(shown.contains("fn parse"), "{shown}");
    assert!(!is_pretty_json(&shown), "{shown}");

    let found = text(dir.path(), &["search", "parse"]);
    assert!(found.contains("parse"), "{found}");
    assert!(!is_pretty_json(&found), "{found}");

    let dest = tempfile::tempdir().expect("workspace dest");
    let added = text(
        dir.path(),
        &[
            "workspace",
            "add",
            "w2",
            dest.path().to_str().expect("utf8 path"),
        ],
    );
    assert!(added.contains("added workspace w2"), "{added}");
    assert!(!is_pretty_json(&added), "{added}");
}

fn json(dir: &Path, args: &[&str]) -> Value {
    let out = bin()
        .current_dir(dir)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
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

#[test]
fn show_def_at_a_past_snapshot_is_the_old_text() {
    let dir = fixture();
    text(dir.path(), &["init"]);
    let at = json(dir.path(), &["status"])["snapshot"]
        .as_str()
        .expect("snapshot")
        .to_string();
    let before = text(dir.path(), &["show-def", "--entity", "parse"]);
    assert!(before.contains("fn parse"), "{before}");
    text(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    let old = text(dir.path(), &["show-def", "--entity", "parse", "--at", &at]);
    assert!(old.contains("fn parse"), "{old}");
    assert!(!old.contains("fn parse_config"), "{old}");
    let now = text(dir.path(), &["show-def", "--entity", "parse_config"]);
    assert!(now.contains("fn parse_config"), "{now}");
}

fn stderr(dir: &Path, args: &[&str]) -> String {
    let out = bin().current_dir(dir).args(args).output().unwrap();
    assert!(
        !out.status.success(),
        "svc {} should fail: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn relocate_rejects_an_escape_path_with_a_reason() {
    let dir = fixture();
    text(dir.path(), &["init"]);
    let parent = stderr(
        dir.path(),
        &[
            "relocate",
            "--entity",
            "parse",
            "--file",
            "../moved.rs",
            "--ordinal",
            "0",
        ],
    );
    assert!(parent.contains("invalid --file ../moved.rs"), "{parent}");
    let abs = stderr(
        dir.path(),
        &[
            "relocate",
            "--entity",
            "parse",
            "--file",
            "/tmp/moved.rs",
            "--ordinal",
            "0",
        ],
    );
    assert!(abs.contains("invalid --file /tmp/moved.rs"), "{abs}");
}
