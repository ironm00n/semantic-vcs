use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, merge, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};
use svc_core::{Conflict, IdentRef};

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
            Conflict::Binding { id, name, was, now, .. } if *id == load => {
                Some((name.clone(), was.clone(), now.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        binds.iter().any(|(name, was, now)| {
            (name == "raw" || name.starts_with('$')) && was != now
        }),
        "expected a Binding conflict on raw, got {:?}",
        merged.conflicts
    );
    assert!(
        binds.iter().any(|(_, was, now)| {
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
    files.insert(path, include_bytes!("../../../demo/config/src/main.rs").to_vec());
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
            Conflict::Binding { id, name, was, now, .. } if *id == load => {
                Some((name.clone(), was.clone(), now.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        merged.conflicts.iter().any(|c| matches!(c, Conflict::Binding { id, .. } if *id == load)),
        "expected Binding on load, got {:?}",
        merged.conflicts
    );
    assert!(
        binds.iter().any(|(name, was, now)| {
            (name == "raw" || name.starts_with('$')) && was != now
        }),
        "expected a Binding conflict on raw, got {:?}",
        merged.conflicts
    );
}
