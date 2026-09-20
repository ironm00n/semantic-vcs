//! `use` lines are opaque entities. Their identity must not depend on where in the
//! file they sit, or every edit above them reads as a semantic change.
use std::collections::BTreeMap;

use svc_core::Kind;
use svc_core::engine::{
    edit_def, lookup_name, merge, rename, render, rust_langs, snapshot_files, status_report,
};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};

const SRC: &str =
    "//! crate doc\nuse std::io;\nuse std::fs;\nfn a() { let _ = (io::stdin(), fs::read); }\n";

fn snap(
    store: &MemStore,
    langs: &svc_core::Langs,
    src: &str,
    prev: Option<&svc_core::Snapshot>,
) -> svc_core::Snapshot {
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), src.as_bytes().to_vec());
    let change = prev.map(|p| p.change).unwrap_or_else(ChangeId::new);
    snapshot_files(store, langs, &files, prev, change).unwrap()
}

#[test]
fn use_lines_keep_identity_when_text_above_them_changes() {
    let store = MemStore::new();
    let langs = rust_langs();
    let s = snap(&store, &langs, SRC, None);
    let uses: Vec<_> = s
        .entities
        .iter()
        .filter(|(_, r)| r.kind == Kind::Opaque)
        .map(|(id, r)| (*id, r.name.clone()))
        .collect();
    assert_eq!(uses.len(), 2);
    assert!(uses.iter().any(|(_, n)| n == "use std::io;"), "{uses:?}");

    let n = snap(
        &store,
        &langs,
        &SRC.replace("crate doc", "crate doc, one more word"),
        Some(&s),
    );
    for (id, _) in &uses {
        assert!(n.entities.contains_key(id), "use line lost its id");
    }
    let rep = status_report(&store, &s, &n).unwrap();
    assert_eq!(rep.semantic, 0, "{:?}", rep.deltas);
}

#[test]
fn both_sides_editing_above_the_uses_does_not_duplicate_them() {
    let store = MemStore::new();
    let langs = rust_langs();
    let base = snap(&store, &langs, SRC, None);
    let a_fn = lookup_name(&base, "a").unwrap();
    // Side A edits `a`; side B re-ingests with a changed doc line. Both keep the two uses.
    let (a, _) = edit_def(
        &store,
        &langs,
        &base,
        a_fn,
        b"fn a() { let _ = (io::stdin(), fs::read, 1); }\n",
    )
    .unwrap();
    let b = snap(
        &store,
        &langs,
        &SRC.replace("crate doc", "crate doc, edited"),
        Some(&base),
    );
    let (bi, ai, bbi) = (
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    );
    let merged = merge(&store, &langs, bi, ai, bbi).unwrap();
    let out = render(&merged, &store, &langs, false).unwrap();
    let text = String::from_utf8(out.files[&RelPath::new("src/lib.rs").unwrap()].clone()).unwrap();
    assert_eq!(text.matches("use std::io;").count(), 1, "{text}");
    assert_eq!(text.matches("use std::fs;").count(), 1, "{text}");
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
}

#[test]
fn a_changed_use_line_keeps_its_id() {
    let store = MemStore::new();
    let langs = rust_langs();
    let before = "mod a;\nuse crate::a::f;\nfn main() { f(); }\n";
    let s = snap(&store, &langs, before, None);
    let use_id = s
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Opaque && r.name.contains("crate::a::f"))
        .map(|(id, _)| *id)
        .expect("use line");
    let after = snap(
        &store,
        &langs,
        "mod a;\nuse crate::a::f as ff;\nfn main() { ff(); }\n",
        Some(&s),
    );
    assert!(after.entities.contains_key(&use_id), "use line reminted");
    assert_eq!(after.entities[&use_id].name, "use crate::a::f as ff;");
    let added_use = after
        .entities
        .iter()
        .filter(|(_, r)| r.kind == Kind::Opaque)
        .count();
    assert_eq!(added_use, 1);
}

#[test]
fn rename_of_an_imported_fn_does_not_remint_the_use() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/a.rs").unwrap(),
        b"pub fn f() {}\n".to_vec(),
    );
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"mod a;\nuse crate::a::f;\nfn main() { f(); }\n".to_vec(),
    );
    let s = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let use_id = s
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Opaque && r.name.contains("crate::a::f"))
        .map(|(id, _)| *id)
        .expect("use line");
    let f = lookup_name(&s, "f").unwrap();
    let renamed = rename(&s, f, "f2").unwrap();
    let rendered = render(&renamed, &store, &langs, false).unwrap();
    let again = snapshot_files(
        &store,
        &langs,
        &rendered.files,
        Some(&renamed),
        ChangeId::new(),
    )
    .unwrap();
    assert!(
        again.entities.contains_key(&use_id),
        "use line reminted after rename"
    );
    assert!(
        again.entities[&use_id].name.contains("f2"),
        "{}",
        again.entities[&use_id].name
    );
}
