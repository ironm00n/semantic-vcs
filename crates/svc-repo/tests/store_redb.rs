use std::collections::BTreeMap;

use svc_core::{
    ChangeId, ChangeSet, Content, Intent, Op, OpIx, OpLogEntry, OpenChangeSet, Snapshot, Store,
    View,
};
use svc_repo::RedbStore;

fn snapshot(change: ChangeId, message: &str) -> Snapshot {
    Snapshot {
        parents: Vec::new(),
        predecessors: Vec::new(),
        change,
        entities: BTreeMap::new(),
        files: BTreeMap::new(),
        conflicts: Vec::new(),
        message: message.into(),
    }
}

fn view(store: &dyn Store) -> View {
    View {
        root: store.root().unwrap(),
        heads: store.heads().unwrap().into_iter().collect(),
    }
}

#[test]
fn every_table_round_trips_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.redb");
    let change = ChangeId::new();
    let (snap_id, blob_id, content_id, op_ix) = {
        let store = RedbStore::create(&path).unwrap();
        assert!(store.root().is_err(), "fresh store has no root");
        assert!(store.heads().unwrap().is_empty());
        assert!(store.ops(OpIx(0), false).unwrap().is_empty());

        let blob_id = store.put_blob(b"hello").unwrap();
        let content_id = store.put_content(&Content::default()).unwrap();
        let snap_id = store.put_snapshot(&snapshot(change, "first")).unwrap();
        store.set_root(snap_id).unwrap();
        store.set_head(change, snap_id).unwrap();
        store.set_branch("main", change).unwrap();
        let entry = OpLogEntry {
            op: Op::Describe { msg: "first".into() },
            observed: None,
            at: 1,
            group: None,
            before: view(&store),
            after: view(&store),
        };
        let op_ix = store.append_op(&entry).unwrap();
        assert_eq!(op_ix, OpIx(0));
        store
            .put_changeset(&ChangeSet {
                id: Default::default(),
                name: "run".into(),
                intent: Intent::Refactor,
                queue: Vec::new(),
                description: String::new(),
            })
            .unwrap();
        store
            .set_open_changeset(Some(OpenChangeSet {
                id: Default::default(),
                pid: None,
                opened_at: 7,
            }))
            .unwrap();
        store.set_render_pending(true).unwrap();
        (snap_id, blob_id, content_id, op_ix)
    };

    let store = RedbStore::open(&path).unwrap();
    assert_eq!(store.get_blob(&blob_id).unwrap(), b"hello");
    assert_eq!(store.get_content(content_id).unwrap(), Content::default());
    assert_eq!(store.get_snapshot(snap_id).unwrap().message, "first");
    assert_eq!(store.root().unwrap(), snap_id);
    assert_eq!(store.head(change).unwrap(), snap_id);
    assert_eq!(store.heads().unwrap(), vec![(change, snap_id)]);
    assert_eq!(store.branch("main").unwrap(), Some(change));
    assert_eq!(store.branch("nope").unwrap(), None);
    assert_eq!(store.branches().unwrap(), vec![("main".to_string(), change)]);
    assert_eq!(store.resolve_prefix(&change.short()).unwrap(), change);
    let ops = store.ops(OpIx(0), false).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].0, op_ix);
    assert!(matches!(ops[0].1.op, Op::Describe { .. }));
    assert_eq!(store.changesets().unwrap().len(), 1);
    assert_eq!(store.open_changeset().unwrap().unwrap().opened_at, 7);
    assert!(store.render_pending().unwrap());
    assert!(matches!(
        store.get_snapshot(svc_core::SnapshotId([9; 32])),
        Err(svc_core::Error::NoSuchSnapshot)
    ));
}

#[test]
fn oplog_is_append_only_and_ordered() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbStore::create(&dir.path().join("s.redb")).unwrap();
    let change = ChangeId::new();
    let id = store.put_snapshot(&snapshot(change, "")).unwrap();
    store.set_root(id).unwrap();
    for i in 0..5u64 {
        let v = view(&store);
        let e = OpLogEntry {
            op: Op::Describe { msg: i.to_string() },
            observed: None,
            at: i,
            group: None,
            before: v.clone(),
            after: v,
        };
        assert_eq!(store.append_op(&e).unwrap(), OpIx(i));
    }
    let fwd = store.ops(OpIx(2), false).unwrap();
    assert_eq!(fwd.iter().map(|(i, _)| i.0).collect::<Vec<_>>(), vec![2, 3, 4]);
    let rev = store.ops(OpIx(0), true).unwrap();
    assert_eq!(rev.iter().map(|(i, _)| i.0).collect::<Vec<_>>(), vec![4, 3, 2, 1, 0]);
}

#[test]
fn evolog_walks_predecessors() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbStore::create(&dir.path().join("s.redb")).unwrap();
    let change = ChangeId::new();
    let v1 = snapshot(change, "v1");
    let v1_id = store.put_snapshot(&v1).unwrap();
    let mut v2 = snapshot(change, "v2");
    v2.predecessors = vec![v1_id];
    let v2_id = store.put_snapshot(&v2).unwrap();
    store.set_head(change, v2_id).unwrap();
    let log = store.evolog(change).unwrap();
    assert_eq!(
        log.iter().map(|s| s.message.as_str()).collect::<Vec<_>>(),
        vec!["v2", "v1"]
    );
}

#[test]
fn two_handles_on_one_store_see_each_others_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.redb");
    let a = RedbStore::create(&path).unwrap();
    let b = RedbStore::open(&path).expect("the store is shared: a second open succeeds");
    let change = ChangeId::new();
    let id = a.put_snapshot(&snapshot(change, "from a")).unwrap();
    a.set_head(change, id).unwrap();
    assert_eq!(b.head(change).unwrap(), id, "b follows a's commit");
    let id2 = b.put_snapshot(&snapshot(change, "from b")).unwrap();
    b.set_head(change, id2).unwrap();
    assert_eq!(a.head(change).unwrap(), id2, "and a follows b's");
}
