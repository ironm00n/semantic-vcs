//! Demo line 6 again, but the merged entity carries a doc comment and an attribute.
//! The rendered entity starts with trivia, so the merge must find the item node by
//! kind rather than taking the first child of the parse root; otherwise the body is
//! one atom (spurious content conflict) and the binding post-condition never runs.
use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, merge, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};
use svc_core::Conflict;

const BASE: &str = r#"fn read(path: &str) -> String { path.to_string() }
fn parse(s: &str) -> String { s.to_string() }
fn normalize(s: &str) -> String { s.trim().to_owned() }
fn log(s: &str) { let _ = s; }
/// Loads a config.
#[inline]
fn load(path: &str) -> String {
    let raw = read(path);
    let cfg = parse(&raw);
    cfg
}
"#;

const A: &[u8] = b"/// Loads a config.
#[inline]
fn load(path: &str) -> String {
    let raw = read(path);
    let raw = normalize(&raw);
    let cfg = parse(&raw);
    cfg
}
";

const B: &[u8] = b"/// Loads a config.
#[inline]
fn load(path: &str) -> String {
    let raw = read(path);
    let cfg = parse(&raw);
    log(&raw);
    cfg
}
";

#[test]
fn o7_binding_conflict_survives_doc_comment_and_attribute() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), BASE.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let load = lookup_name(&base, "load").unwrap();
    let (a, _) = edit_def(&store, &langs, &base, load, A).unwrap();
    let (b, _) = edit_def(&store, &langs, &base, load, B).unwrap();
    let base_id = store.put_snapshot(&base).unwrap();
    let a_id = store.put_snapshot(&a).unwrap();
    let b_id = store.put_snapshot(&b).unwrap();

    let merged = merge(&store, &langs, base_id, a_id, b_id).unwrap();

    assert!(
        !merged.conflicts.iter().any(|c| matches!(c, Conflict::Content { .. })),
        "non-overlapping statement edits must merge at atom level: {:?}",
        merged.conflicts
    );
    assert!(
        merged
            .conflicts
            .iter()
            .any(|c| matches!(c, Conflict::Binding { id, was, now, .. } if *id == load && was != now)),
        "expected a binding conflict on `raw` in load: {:?}",
        merged.conflicts
    );
    let out = render(&merged, &store, &langs, false).unwrap();
    let text = String::from_utf8(out.files[&path].clone()).unwrap();
    assert!(text.contains("let raw = normalize(&raw);") && text.contains("log(&raw);"), "{text}");
    assert!(text.contains("/// Loads a config.\n#[inline]\nfn load"), "{text}");
}
