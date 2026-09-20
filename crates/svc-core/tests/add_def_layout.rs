//! A definition added into an impl renders as a separate, indented member — not glued
//! to the previous method's closing brace.
use std::collections::BTreeMap;

use svc_core::engine::{add_def, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Intent, Kind};

#[test]
fn nested_add_def_is_on_its_own_indented_line() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "pub struct B(Vec<u8>);\n\nimpl B {\n    pub fn len(&self) -> usize {\n        self.0.len()\n    }\n}\n";
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let next = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        Some(imp),
        1,
        b"pub fn is_empty(&self) -> bool {\n    self.0.is_empty()\n}",
        Intent::Feature,
    )
    .unwrap();
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone())
        .unwrap();
    assert!(
        !out.contains("}pub fn"),
        "glued to the previous member:\n{out}"
    );
    assert!(
        out.contains(
            "    }\n\n    pub fn is_empty(&self) -> bool {\n        self.0.is_empty()\n    }\n}\n"
        ),
        "no blank line before the closing brace:\n{out}"
    );
    let reparsed = snapshot_files(
        &store,
        &langs,
        &BTreeMap::from([(path, out.into_bytes())]),
        Some(&next),
        next.change,
    )
    .unwrap();
    assert!(
        reparsed
            .entities
            .values()
            .any(|r| r.name == "is_empty" && r.parent == Some(imp)),
        "re-ingest keeps the member nested"
    );
}
