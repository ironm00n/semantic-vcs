//! O4: `undo(op(s)) == s` by restoring the recorded `View` (the oracle set).
//!
//! The repo verb (`svc undo`) also re-renders and groups by changeset; this
//! oracle checks the store law those verbs rest on: after an op, `set_root`
//! + `set_head` from `entry.before` yields the original snapshot hash.

use std::collections::BTreeMap;

use svc_core::engine::{lookup_name, rename, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, ChangeSetId, RelPath};
use svc_core::op::{Op, OpLogEntry, View};
use svc_core::store::MemStore;
use svc_core::Store;

const SRC: &str = r#"fn parse(s: &str) -> usize { s.len() }
fn load(path: &str) -> usize { parse(path) }
"#;

fn view(store: &MemStore) -> View {
    View {
        root: store.root().unwrap(),
        heads: store.heads().unwrap().into_iter().collect(),
    }
}

fn commit(store: &MemStore, snap: &svc_core::Snapshot) {
    let id = store.put_snapshot(snap).unwrap();
    store.set_root(id).unwrap();
    store.set_head(snap.change, id).unwrap();
}

fn record(store: &MemStore, op: Op, before: View, after: View, group: Option<ChangeSetId>) {
    store
        .append_op(&OpLogEntry {
            op,
            observed: None,
            at: 0,
            group,
            before,
            after,
        })
        .unwrap();
}

fn undo_last(store: &MemStore) {
    let ops = store.ops(svc_core::ids::OpIx(0), true).unwrap();
    let (_, last) = ops.first().expect("oplog");
    if let Some(group) = last.group {
        let mut earliest = last.before.clone();
        for (_, e) in store.ops(svc_core::ids::OpIx(0), false).unwrap() {
            if e.group == Some(group) {
                earliest = e.before;
                break;
            }
        }
        store.set_root(earliest.root).unwrap();
        for (change, snap) in earliest.heads {
            store.set_head(change, snap).unwrap();
        }
        return;
    }
    store.set_root(last.before.root).unwrap();
    for (change, snap) in &last.before.heads {
        store.set_head(*change, *snap).unwrap();
    }
}

#[test]
fn o4_undo_rename_restores_snapshot_hash() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let change = ChangeId::new();
    let snap = snapshot_files(&store, &langs, &files, None, change).unwrap();
    commit(&store, &snap);
    let origin = store.root().unwrap();

    let parse = lookup_name(&snap, "parse").unwrap();
    let before = view(&store);
    let next = rename(&store, &snap, parse, "parse_config").unwrap();
    commit(&store, &next);
    record(
        &store,
        Op::Rename {
            id: parse,
            new: "parse_config".into(),
        },
        before,
        view(&store),
        None,
    );

    assert_ne!(store.root().unwrap(), origin);
    undo_last(&store);
    assert_eq!(store.root().unwrap(), origin, "undo must restore before.root");
    let restored = store.get_snapshot(store.root().unwrap()).unwrap();
    assert_eq!(restored.entities[&parse].name, "parse");
}

#[test]
fn o4_changeset_undo_restores_the_group_not_one_op() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let change = ChangeId::new();
    let snap = snapshot_files(&store, &langs, &files, None, change).unwrap();
    commit(&store, &snap);
    let origin = store.root().unwrap();
    let group = Some(ChangeSetId::new());

    let parse = lookup_name(&snap, "parse").unwrap();
    let load = lookup_name(&snap, "load").unwrap();

    let before1 = view(&store);
    let s1 = rename(&store, &snap, parse, "parse_config").unwrap();
    commit(&store, &s1);
    record(
        &store,
        Op::Rename {
            id: parse,
            new: "parse_config".into(),
        },
        before1,
        view(&store),
        group,
    );

    let before2 = view(&store);
    let s2 = rename(&store, &s1, load, "load_cfg").unwrap();
    commit(&store, &s2);
    record(
        &store,
        Op::Rename {
            id: load,
            new: "load_cfg".into(),
        },
        before2,
        view(&store),
        group,
    );

    undo_last(&store);
    assert_eq!(
        store.root().unwrap(),
        origin,
        "group undo must drop both ops, not only the last"
    );
}
