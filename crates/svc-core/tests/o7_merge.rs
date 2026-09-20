use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, merge, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::lang::Langs;
use svc_core::store::{MemStore, Store};
use svc_core::{Conflict, IdentRef, JsLang, Kind, RustLang};

const BASE: &str = r#"
fn read(path: &str) -> String { path.to_string() }
fn parse(s: &str) -> String { s.to_string() }
fn normalize(s: &str) -> String { s.trim().to_owned() }
fn log(s: &str) { let _ = s; }
fn load(path: &str) -> String {
    let raw = read(path);
    let cfg = parse(&raw);
    cfg
}
"#;

#[test]
fn o7_demo_line_6_is_a_binding_conflict_on_raw() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, BASE.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let load = lookup_name(&base, "load").unwrap();

    let a_src = br#"fn load(path: &str) -> String {
    let raw = read(path);
    let raw = normalize(&raw);
    let cfg = parse(&raw);
    cfg
}
"#;
    let b_src = br#"fn load(path: &str) -> String {
    let raw = read(path);
    let cfg = parse(&raw);
    log(&raw);
    cfg
}
"#;
    let (a, _) = edit_def(&store, &langs, &base, load, a_src).unwrap();
    let (b, _) = edit_def(&store, &langs, &base, load, b_src).unwrap();
    let base_id = store.put_snapshot(&base).unwrap();
    let a_id = store.put_snapshot(&a).unwrap();
    let b_id = store.put_snapshot(&b).unwrap();
    let merged = merge(&store, &langs, base_id, a_id, b_id).unwrap();
    let binds: Vec<_> = merged
        .conflicts
        .iter()
        .filter_map(|c| match c {
            Conflict::Binding {
                id,
                name,
                was,
                was_at,
                now,
                ..
            } if *id == load => Some((name.clone(), was.clone(), now.clone(), was_at.is_some())),
            _ => None,
        })
        .collect();
    assert!(
        binds
            .iter()
            .any(|(name, was, now, located)| { name == "raw" && was != now && *located }),
        "expected a Binding conflict on raw with was_at, got {:?}",
        merged.conflicts
    );
    assert!(
        binds.iter().any(|(_, was, now, _)| {
            matches!(
                (was, now),
                (IdentRef::Local(a, _), IdentRef::Local(b, _)) if a != b
            )
        }),
        "capture should move the local slot: {binds:?}"
    );
}

const LOAD_A: &[u8] = b"fn load(path: &str) -> Result<Config, Error> {
    let raw = read(path);
    let raw = normalize(&raw);
    let cfg = parse(&raw)?;
    validate(&cfg)?;
    let _path_exists = !cfg.path.is_empty();
    let _retry_count = cfg.retries;
    Ok(cfg)
}";

const LOAD_B: &[u8] = b"fn load(path: &str) -> Result<Config, Error> {
    let raw = read(path);
    let cfg = parse(&raw)?;
    validate(&cfg)?;
    let _path_exists = !cfg.path.is_empty();
    let _retry_count = cfg.retries;
    log(&raw);
    Ok(cfg)
}";

#[test]
fn o7_git_twin_load_is_a_binding_conflict_on_raw() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/main.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path,
        include_bytes!("../../../demo/config/src/main.rs").to_vec(),
    );
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let load = lookup_name(&base, "load").unwrap();
    assert_ne!(LOAD_A.last(), Some(&b'\n'));
    assert_ne!(LOAD_B.last(), Some(&b'\n'));
    let (a, _) = edit_def(&store, &langs, &base, load, LOAD_A).unwrap();
    let (b, _) = edit_def(&store, &langs, &base, load, LOAD_B).unwrap();
    let base_id = store.put_snapshot(&base).unwrap();
    let a_id = store.put_snapshot(&a).unwrap();
    let b_id = store.put_snapshot(&b).unwrap();
    let merged = merge(&store, &langs, base_id, a_id, b_id).unwrap();
    let binds: Vec<_> = merged
        .conflicts
        .iter()
        .filter_map(|c| match c {
            Conflict::Binding {
                id,
                name,
                was,
                was_at,
                now,
                ..
            } if *id == load => Some((name.clone(), was.clone(), now.clone(), was_at.is_some())),
            _ => None,
        })
        .collect();
    assert!(
        merged
            .conflicts
            .iter()
            .any(|c| matches!(c, Conflict::Binding { id, .. } if *id == load)),
        "expected Binding on load, got {:?}",
        merged.conflicts
    );
    assert!(
        binds
            .iter()
            .any(|(name, was, now, located)| { name == "raw" && was != now && *located }),
        "expected a Binding conflict on raw with was_at, got {:?}",
        merged.conflicts
    );
}

fn js_langs() -> Langs {
    Langs::new(vec![Box::new(RustLang), Box::new(JsLang)])
}

/// Comment-only both-sides on the JS twin: constructor param `path` shares a
/// name with `src/main.js`'s `const path`. Re-resolution must not treat the
/// param as the module binding.
#[test]
fn js_comment_only_both_sides_is_not_a_binding_conflict() {
    let store = MemStore::new();
    let langs = js_langs();
    let cfg = RelPath::new("src/config.js").unwrap();
    let main = RelPath::new("src/main.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        cfg.clone(),
        include_bytes!("../../../demo/config-js/src/config.js").to_vec(),
    );
    files.insert(
        main.clone(),
        include_bytes!("../../../demo/config-js/src/main.js").to_vec(),
    );
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let mut a_files = files.clone();
    a_files
        .get_mut(&cfg)
        .unwrap()
        .extend_from_slice(b"\n// a\n");
    let mut b_files = files.clone();
    b_files
        .get_mut(&cfg)
        .unwrap()
        .extend_from_slice(b"\n// b\n");
    let a = snapshot_files(&store, &langs, &a_files, Some(&base), ChangeId::new()).unwrap();
    let b = snapshot_files(&store, &langs, &b_files, Some(&base), ChangeId::new()).unwrap();
    let merged = merge(
        &store,
        &langs,
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    )
    .unwrap();
    let binds: Vec<_> = merged
        .conflicts
        .iter()
        .filter(|c| matches!(c, Conflict::Binding { .. }))
        .collect();
    assert!(
        binds.is_empty(),
        "comment-only JS merge must not invent Binding conflicts: {binds:?}"
    );
}

#[test]
fn object_literal_execute_methods_are_not_class_members_and_do_not_add_add() {
    let js = concat!(
        "export function apply(ctx) {\n",
        "  ctx.tools.register({ async execute(args) { return 1; } });\n",
        "  ctx.tools.register({ async execute(args) { return 2; } });\n",
        "}\n",
        "class Tool { async execute(args) { return 3; } }\n",
    );
    let rust = "fn a() {}\nfn b() {}\n";
    let store = MemStore::new();
    let langs = js_langs();
    let js_path = RelPath::new("harness/svc-tools.mjs").unwrap();
    let rs_path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(js_path, js.as_bytes().to_vec());
    files.insert(rs_path.clone(), rust.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let object_executes = base
        .entities
        .values()
        .filter(|r| r.name == "execute" && r.parent.is_some())
        .count();
    assert_eq!(
        object_executes,
        1,
        "only the class method is an entity, got {:?}",
        base.entities
            .values()
            .filter(|r| r.name == "execute")
            .map(|r| (r.kind, r.parent, r.ordinal))
            .collect::<Vec<_>>()
    );
    assert!(
        base.entities
            .values()
            .any(|r| r.name == "execute" && r.kind == Kind::JsMethod)
    );

    let mut a_files = files.clone();
    a_files.insert(rs_path.clone(), b"fn a() { 1 }\nfn b() {}\n".to_vec());
    let mut b_files = files.clone();
    b_files.insert(rs_path, b"fn a() {}\nfn b() { 2 }\n".to_vec());
    let a = snapshot_files(&store, &langs, &a_files, Some(&base), ChangeId::new()).unwrap();
    let b = snapshot_files(&store, &langs, &b_files, Some(&base), ChangeId::new()).unwrap();
    let merged = merge(
        &store,
        &langs,
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    )
    .unwrap();
    let add_add: Vec<_> = merged
        .conflicts
        .iter()
        .filter(|c| matches!(c, Conflict::AddAdd { .. }))
        .collect();
    assert!(
        add_add.is_empty(),
        "unrelated rust edits must not AddAdd object-literal execute: {add_add:?}"
    );
}

#[test]
fn merge_does_not_flag_same_named_calls_in_other_files() {
    let store = MemStore::new();
    let langs = rust_langs();
    let a_path = RelPath::new("crates/svc-core/src/a.rs").unwrap();
    let b_path = RelPath::new("crates/svc-core/src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a_path.clone(), b"fn run() {}\nfn a() { run(); }\n".to_vec());
    files.insert(b_path.clone(), b"fn run() {}\nfn b() { run(); }\n".to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let mut a_files = files.clone();
    a_files.insert(a_path, b"fn run() {}\nfn a() { run(); /* a */ }\n".to_vec());
    let mut b_files = files.clone();
    b_files.insert(b_path, b"fn run() {}\nfn b() { run(); /* b */ }\n".to_vec());
    let a = snapshot_files(&store, &langs, &a_files, Some(&base), ChangeId::new()).unwrap();
    let b = snapshot_files(&store, &langs, &b_files, Some(&base), ChangeId::new()).unwrap();
    let merged = merge(
        &store,
        &langs,
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    )
    .unwrap();
    let binds: Vec<_> = merged
        .conflicts
        .iter()
        .filter(|c| matches!(c, Conflict::Binding { .. }))
        .collect();
    assert!(
        binds.is_empty(),
        "file-local calls must not rebind at merge: {binds:?}"
    );
}

#[test]
fn merge_flags_when_one_side_deletes_a_callee_the_other_still_calls() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn helper(x: u32) -> u32 { x + 1 }\nfn other() -> u32 { 7 }\nfn caller() -> u32 { helper(other()) }\n"
            .to_vec(),
    );
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let caller = lookup_name(&base, "caller").unwrap();
    let helper = lookup_name(&base, "helper").unwrap();
    let mut a_files = files.clone();
    a_files.insert(
        path,
        b"fn other() -> u32 { 7 }\nfn caller() -> u32 { helper(other()) }\n".to_vec(),
    );
    let a = snapshot_files(&store, &langs, &a_files, Some(&base), ChangeId::new()).unwrap();
    let b_src = b"fn caller() -> u32 { helper(other()) + 1 }\n";
    let (b, _) = edit_def(&store, &langs, &base, caller, b_src).unwrap();
    let merged = merge(
        &store,
        &langs,
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    )
    .unwrap();
    assert!(
        merged.conflicts.iter().any(|c| matches!(
            c,
            Conflict::Binding { id, was, now, .. }
            if *id == caller
                && matches!(was, IdentRef::Entity(h) if *h == helper)
                && matches!(now, IdentRef::Free(n) if n.as_ref() == "helper")
        )),
        "delete+edit of a live call must not merge clean: {:?}",
        merged.conflicts
    );
}
