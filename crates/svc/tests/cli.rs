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
    bin()
        .current_dir(dir)
        .args(args)
        .arg("--json")
        .output()
        .unwrap()
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
    assert!(
        err["error"].as_str().unwrap().contains("already exists"),
        "{err}"
    );
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
        rows.iter()
            .any(|r| r["name"] == "default" && r["current"] == true),
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
        after
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != "agent"),
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
        evo.as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"] == "first change"),
        "{evo}"
    );
}

#[test]
fn diff_two_changes_and_render() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    let heads = json(dir.path(), &["heads"]);
    let rows = heads.as_array().unwrap();
    let base = rows.iter().find(|h| h["current"] == false).unwrap()["change"]
        .as_str()
        .unwrap()
        .to_string();
    json(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    let cur = json(dir.path(), &["status"])["change"]
        .as_str()
        .unwrap()
        .to_string();
    let diffed = json(dir.path(), &["diff", &base, &cur]);
    let deltas = diffed["deltas"].as_array().expect("deltas");
    assert!(
        deltas.iter().any(|d| d.get("Renamed").is_some()),
        "{diffed}"
    );
    let rendered = json(dir.path(), &["render"]);
    assert_eq!(rendered["rendered"], true, "{rendered}");
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
        blame
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["op"].get("Rename").is_some()),
        "{blame}"
    );
    json(dir.path(), &["undo"]);
    let after = json(dir.path(), &["log"]);
    assert_eq!(after[0]["op"], "Undo", "{after}");
    let defs = json(dir.path(), &["list-defs"]);
    assert!(
        defs["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["name"] == "parse"),
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
        defs["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["name"] == "helper"),
        "{defs}"
    );
    let status = json(dir.path(), &["changeset", "status"]);
    assert_eq!(status["name"], "run");
    assert!(!status["ops"].as_array().unwrap().is_empty(), "{status}");
    json(
        dir.path(),
        &["delete", "--entity", "helper", "--intent", "refactor"],
    );
    json(dir.path(), &["changeset", "end"]);
    assert!(json(dir.path(), &["changeset", "status"]).is_null());
    let defs = json(dir.path(), &["list-defs"]);
    assert!(
        defs["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["name"] != "helper"),
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

#[test]
fn op_log_forge_export_and_classify() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    let ops = json(dir.path(), &["op", "log"]);
    assert!(
        ops.as_array()
            .unwrap()
            .iter()
            .any(|e| e["op"].get("Rename").is_some()),
        "{ops}"
    );
    let catalog_path = dir.path().join(".svc/forge.json");
    let exported = json(dir.path(), &["forge", "export"]);
    assert!(
        exported["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("forge.json")),
        "{exported}"
    );
    assert!(catalog_path.is_file(), "missing {}", catalog_path.display());
    let catalog: Value = serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    let n_ops = ops.as_array().unwrap().len();
    assert_eq!(
        catalog["repositories"][0]["operations"]
            .as_array()
            .unwrap()
            .len(),
        n_ops,
        "{catalog}"
    );
    let classed = json(
        dir.path(),
        &[
            "classify",
            "--entity",
            "parse_config",
            "--definition",
            "fn parse_config(s: &str) -> String { s.to_string() }\n",
        ],
    );
    assert!(
        classed.get("class").is_some()
            || classed.get("observed").is_some()
            || classed.as_str().is_some(),
        "{classed}"
    );
}

fn rust_item(dir: &Path, name: &str) -> String {
    let src = fs::read_to_string(dir.join("src/main.rs")).unwrap();
    let needle = format!("fn {name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("no {needle} in {src}"));
    let rest = &src[start..];
    let mut depth = 0i32;
    let mut seen = false;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => {
                depth += 1;
                seen = true;
            }
            '}' => {
                depth -= 1;
                if seen && depth == 0 {
                    return rest[..=i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unclosed {name}");
}

#[test]
fn edit_def_and_rename_merge_is_clean() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(
        dir.path(),
        &["rename", "--entity", "parse", "--new-name", "parse_config"],
    );
    json(dir.path(), &["new"]);
    json(dir.path(), &["branch", "a"]);
    json(
        dir.path(),
        &[
            "rename",
            "--entity",
            "parse_config",
            "--new-name",
            "parse_cfg",
        ],
    );
    json(dir.path(), &["branch", "b"]);
    let main_b = rust_item(dir.path(), "main").replacen(
        "    match load(&path) {",
        "    let _ = parse_config(\"x\");\n    match load(&path) {",
        1,
    );
    json(
        dir.path(),
        &[
            "edit-def",
            "--entity",
            "main",
            "--intent",
            "feature",
            "--definition",
            &main_b,
        ],
    );
    let merged = json(dir.path(), &["merge", "a"]);
    assert_eq!(merged["conflicts"].as_array().unwrap().len(), 0, "{merged}");
    let rendered = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(rendered.contains("let _ = parse_cfg(\"x\")"), "{rendered}");
}

#[test]
fn both_sides_edit_load_is_a_binding_conflict() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    json(dir.path(), &["branch", "a6"]);
    let load_a = rust_item(dir.path(), "load").replacen(
        "    let cfg = parse(&raw)?;",
        "    let raw = normalize(&raw);\n    let cfg = parse(&raw)?;",
        1,
    );
    json(
        dir.path(),
        &[
            "edit-def",
            "--entity",
            "load",
            "--intent",
            "feature",
            "--definition",
            &load_a,
        ],
    );
    json(dir.path(), &["branch", "b6"]);
    let load_b =
        rust_item(dir.path(), "load").replacen("    Ok(cfg)", "    log(&raw);\n    Ok(cfg)", 1);
    json(
        dir.path(),
        &[
            "edit-def",
            "--entity",
            "load",
            "--intent",
            "feature",
            "--definition",
            &load_b,
        ],
    );
    let merged = json(dir.path(), &["merge", "a6"]);
    let conflicts = merged["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "{merged}");
    assert_eq!(conflicts[0]["name"], "load", "{merged}");
    assert!(
        conflicts[0]["conflict"].get("Binding").is_some(),
        "{merged}"
    );
    let listed = json(dir.path(), &["conflicts"]);
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    let src = rust_item(dir.path(), "load");
    assert!(src.contains("normalize(&raw)"), "{src}");
    assert!(src.contains("log(&raw)"), "{src}");
}

#[test]
fn relocate_to_a_new_file_writes_that_file() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    json(
        dir.path(),
        &[
            "relocate",
            "--entity",
            "log",
            "--file",
            "src/log.rs",
            "--ordinal",
            "0",
        ],
    );
    let dest = fs::read_to_string(dir.path().join("src/log.rs")).unwrap();
    assert!(dest.contains("fn log"), "{dest}");
    let main = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(!main.contains("fn log("), "{main}");
}

#[test]
fn add_def_to_a_new_file_writes_that_file() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    json(
        dir.path(),
        &[
            "add-def",
            "--ordinal",
            "0",
            "--intent",
            "feature",
            "--file",
            "src/extra.rs",
            "--definition",
            "fn extra() {}\n",
        ],
    );
    let dest = fs::read_to_string(dir.path().join("src/extra.rs")).unwrap();
    assert!(dest.contains("fn extra"), "{dest}");
    let main = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(!main.contains("fn extra"), "{main}");
}

#[test]
fn extract_hoists_a_nested_method_to_file_root() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    let defs = json(dir.path(), &["list-defs"]);
    let nested = defs["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "fmt" && !d["parent"].is_null())
        .unwrap();
    let id = nested["id"].as_str().unwrap().to_string();
    json(dir.path(), &["extract", "--entity", &id]);
    let after = json(dir.path(), &["list-defs"]);
    let row = after["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap();
    assert!(row["parent"].is_null(), "{row}");
    let src = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(src.contains("fn fmt"), "{src}");
}

#[test]
fn move_a_fn_into_an_impl_renders_inside_it() {
    let dir = fixture();
    json(dir.path(), &["init"]);
    json(dir.path(), &["new"]);
    let defs = json(dir.path(), &["list-defs"]);
    let imp = defs["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "Impl" && d["name"].as_str().unwrap().contains("Config"))
        .unwrap();
    let imp_id = imp["id"].as_str().unwrap().to_string();
    json(
        dir.path(),
        &["move", "--entity", "log", "--new-parent", &imp_id, "--ordinal", "1"],
    );
    let src = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    let start = src
        .find("impl fmt::Display for Config")
        .expect(&format!("no Config Display impl in {src}"));
    let block = &src[start..];
    let end = block.find("\n}\n").unwrap_or(block.len());
    assert!(
        block[..end].contains("fn log"),
        "log should render inside the Config impl:\n{src}"
    );
}
