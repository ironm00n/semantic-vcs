//! An item moved between nesting levels is re-indented: a method extracted to file level
//! starts at column 0, a free fn moved into an impl gains the members' indentation.
//! Every line follows (bodies, attributes, doc comments), and the result reparses.
use std::collections::BTreeMap;

use svc_core::engine::{extract_hoist, lookup_name, move_def, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::Kind;

const SRC: &str = "\
pub struct B(Vec<u8>);

impl B {
    /// Length.
    #[inline]
    pub fn len(&self) -> usize {
        if self.0.is_empty() {
            0
        } else {
            self.0.len()
        }
    }
}

fn helper(x: u32) -> u32 {
    x + 1
}
";

fn setup() -> (MemStore, svc_core::Snapshot, RelPath) {
    let store = MemStore::new();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &rust_langs(), &files, None, ChangeId::new()).unwrap();
    (store, snap, path)
}

fn text(store: &MemStore, snap: &svc_core::Snapshot, path: &RelPath) -> String {
    String::from_utf8(render(snap, store, &rust_langs(), false).unwrap().files[path].clone()).unwrap()
}

#[test]
fn extracting_a_method_to_file_level_dedents_it() {
    let (store, snap, path) = setup();
    let len = lookup_name(&snap, "len").unwrap();
    let next = extract_hoist(&store, &rust_langs(), &snap, len, None, 2).unwrap();
    let out = text(&store, &next, &path);
    assert!(
        out.contains("\n/// Length.\n#[inline]\npub fn len(&self) -> usize {\n    if self.0.is_empty() {\n        0\n    } else {\n        self.0.len()\n    }\n}\n"),
        "{out}"
    );
    assert!(!out.contains("\n    /// Length."), "{out}");
    let reparsed = snapshot_files(
        &store,
        &rust_langs(),
        &BTreeMap::from([(path.clone(), out.into_bytes())]),
        Some(&next),
        ChangeId::new(),
    )
    .unwrap();
    assert_eq!(reparsed.entities.values().filter(|r| r.kind == Kind::Fn).count(), 2);
}

#[test]
fn moving_a_free_fn_into_an_impl_indents_it() {
    let (store, snap, path) = setup();
    let helper = lookup_name(&snap, "helper").unwrap();
    let imp = snap.entities.iter().find(|(_, r)| r.kind == Kind::Impl).map(|(id, _)| *id).unwrap();
    let next = move_def(&store, &rust_langs(), &snap, helper, Some(imp), None).unwrap();
    let out = text(&store, &next, &path);
    assert!(
        out.contains("    }\n\n    fn helper(x: u32) -> u32 {\n        x + 1\n    }\n}\n"),
        "{out}"
    );
    assert!(!out.contains("\nfn helper"), "{out}");
}
