use std::path::Path;

use svc_core::{Intent, Op, OpIx, OpenChangeSet};
use svc_repo::{Repo, Touch};

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

#[test]
fn init_snapshots_tracked_files_only_and_renders_clean() {
    let (dir, repo) = fresh();
    let snap = repo.current().unwrap();
    assert_eq!(snap.files.len(), 1, "Cargo.toml is not tracked");
    assert_eq!(snap.entities.len(), 3);
    assert!(repo.working_copy_clean().unwrap());
    assert_eq!(svc_repo::op_log(&repo).unwrap().len(), 1);
    assert!(Repo::init(dir.path(), Repo::default_langs()).is_err(), "no re-init");
    let found = Repo::find_root(&dir.path().join("src")).unwrap();
    assert_eq!(found, dir.path());
    let _ = svc_repo::heads(&repo);
}

#[test]
fn new_starts_a_change_and_log_is_scoped_to_it() {
    let (_dir, repo) = fresh();
    let first = repo.current_change().unwrap();
    let out = svc_repo::new(&repo).unwrap();
    let second = repo.current_change().unwrap();
    assert_ne!(first, second);
    assert_eq!(out.change, second);
    let cur = repo.current().unwrap();
    assert_eq!(cur.parents, vec![repo.store().head(first).unwrap()]);
    assert!(cur.predecessors.is_empty());

    let heads = svc_repo::heads(&repo).unwrap();
    assert_eq!(heads.len(), 2);
    assert!(heads[0].current && heads[0].change == second);

    let log = svc_repo::log(&repo, None).unwrap();
    assert_eq!(log.len(), 1);
    assert!(matches!(log[0].op, Op::New { change } if change == second));
    let old = svc_repo::log(&repo, Some(first)).unwrap();
    assert_eq!(old.len(), 1, "the init op moved the first change's head; `new` did not");
    assert_eq!(svc_repo::op_log(&repo).unwrap().len(), 2);
}

#[test]
fn describe_amends_and_identical_amend_is_a_noop() {
    let (_dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let change = repo.current_change().unwrap();
    let before = repo.store().root().unwrap();
    svc_repo::describe(&repo, "rename things").unwrap();
    let after = repo.store().root().unwrap();
    assert_ne!(before, after);
    assert_eq!(repo.current().unwrap().predecessors, vec![before]);

    let same = svc_repo::describe(&repo, "rename things").unwrap();
    assert_eq!(same.snapshot, after, "identical content is not rewritten");
    assert_eq!(repo.store().root().unwrap(), after);

    let evolog = svc_repo::evolog(&repo, change).unwrap();
    assert_eq!(evolog.len(), 2, "two entries, not three");
    assert_eq!(evolog[0].message, "rename things");
    assert_eq!(evolog[1].message, "");
    assert!(evolog[0].deltas.is_empty(), "a message change touches no entity");
}

#[test]
fn rename_via_mutate_renders_and_is_visible_in_log_blame_evolog() {
    let (dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let change = repo.current_change().unwrap();
    let cur = repo.current().unwrap();
    let (id, _) = cur
        .entities
        .iter()
        .find(|(_, r)| r.name == "read")
        .map(|(id, r)| (*id, r.clone()))
        .unwrap();
    let m = repo
        .mutate(
            Op::Rename { id, new: "read_file".into() },
            None,
            |repo, cur| {
                let mut next = cur.clone();
                next.entities.get_mut(&id).unwrap().name = "read_file".into();
                repo.amend(cur, next)
            },
        )
        .unwrap();
    assert_eq!(m.ix, OpIx(2));

    let text = read(dir.path(), "src/lib.rs");
    assert!(text.contains("fn read_file(path: &str)"), "{text}");
    assert!(!text.contains("fn read("), "{text}");
    assert!(text.contains("read(\"x\")"), "callers are literal until resolve lands: {text}");
    assert!(repo.working_copy_clean().unwrap());

    let log = svc_repo::log(&repo, None).unwrap();
    assert_eq!(log.len(), 2);
    assert!(matches!(&log[0].op, Op::Rename { new, .. } if new == "read_file"));
    assert!(!log[0].flagged);

    let blame = svc_repo::blame(&repo, id).unwrap();
    assert_eq!(blame.len(), 2, "rename, then the init that added it");
    assert_eq!(
        blame[0].touch,
        Touch::Renamed { from: "read".into(), to: "read_file".into() }
    );
    assert_eq!(blame[1].touch, Touch::Added);

    let evolog = svc_repo::evolog(&repo, change).unwrap();
    assert_eq!(evolog.len(), 2);
    assert_eq!(evolog[0].deltas.len(), 1);
    assert_eq!(evolog[0].deltas[0].name, "read_file");
}

#[test]
fn branch_is_a_sibling_of_the_current_change() {
    let (_dir, repo) = fresh();
    let root_change = repo.current_change().unwrap();
    let root_snap = repo.store().root().unwrap();
    svc_repo::new(&repo).unwrap();
    svc_repo::describe(&repo, "work in progress").unwrap();
    let wip = repo.current_change().unwrap();

    let a = svc_repo::branch(&repo, "a").unwrap();
    assert_eq!(repo.resolve_change("a").unwrap(), a.change);
    assert_eq!(repo.resolve_change(&a.change.short()).unwrap(), a.change);
    let cur = repo.current().unwrap();
    assert_eq!(cur.change, a.change);
    assert_eq!(cur.parents, vec![root_snap], "parent is the wip change's parent");
    assert_eq!(cur.message, "");
    assert!(svc_repo::branch(&repo, "a").is_err());

    let heads = svc_repo::heads(&repo).unwrap();
    assert_eq!(heads.len(), 3);
    assert_eq!(heads[0].branches, vec!["a".to_string()]);
    assert!(heads.iter().any(|h| h.change == wip && h.message == "work in progress"));

    svc_repo::edit(&repo, "wip-does-not-exist").unwrap_err();
    svc_repo::edit(&repo, &wip.short()).unwrap();
    assert_eq!(repo.current_change().unwrap(), wip);
    let _ = root_change;
}

#[test]
fn undo_restores_the_view_and_redo_is_undo_again() {
    let (dir, repo) = fresh();
    let v0 = repo.view().unwrap();
    svc_repo::new(&repo).unwrap();
    let v1 = repo.view().unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn other() {}\n").unwrap();
    // Hand edits would be absorbed first; there is no `status` yet, so put them back.
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();

    let u = svc_repo::undo(&repo).unwrap();
    assert_eq!(repo.view().unwrap().root, v0.root);
    assert_eq!(u.snapshot, v0.root);
    let ops = svc_repo::op_log(&repo).unwrap();
    assert!(matches!(ops[0].op, Op::Undo));
    assert_eq!(read(dir.path(), "src/lib.rs"), LIB);

    svc_repo::undo(&repo).unwrap();
    assert_eq!(repo.view().unwrap().root, v1.root);

    svc_repo::op_restore(&repo, OpIx(0)).unwrap();
    assert_eq!(repo.view().unwrap().root, v0.root);
}

#[test]
fn undo_drops_an_open_changeset_whole() {
    let (_dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let start = repo.view().unwrap();
    let cs = svc_core::ChangeSet {
        id: Default::default(),
        name: "agent run".into(),
        intent: Intent::Refactor,
        queue: Vec::new(),
        description: String::new(),
    };
    repo.store().put_changeset(&cs).unwrap();
    repo.store()
        .set_open_changeset(Some(OpenChangeSet {
            id: cs.id,
            pid: None,
            opened_at: svc_repo::repo::now(),
        }))
        .unwrap();
    for msg in ["one", "two", "three"] {
        let m = svc_repo::describe(&repo, msg).unwrap();
        assert!(m.closed_stale_changeset.is_none());
    }
    repo.store().set_open_changeset(None).unwrap();
    assert_eq!(repo.current().unwrap().message, "three");
    let grouped = svc_repo::op_log(&repo)
        .unwrap()
        .iter()
        .filter(|o| o.group == Some(cs.id))
        .count();
    assert_eq!(grouped, 3);

    svc_repo::undo(&repo).unwrap();
    assert_eq!(repo.view().unwrap().root, start.root, "all three ops undone in one step");
    assert_eq!(repo.current().unwrap().message, "");
}

#[test]
fn stale_open_changeset_is_closed_and_reported() {
    let (_dir, repo) = fresh();
    repo.store()
        .set_open_changeset(Some(OpenChangeSet {
            id: Default::default(),
            pid: Some(u32::MAX - 1),
            opened_at: 0,
        }))
        .unwrap();
    let m = svc_repo::describe(&repo, "x").unwrap();
    assert!(m.closed_stale_changeset.is_some());
    assert!(repo.store().open_changeset().unwrap().is_none());
    assert!(svc_repo::op_log(&repo).unwrap()[0].group.is_none());
}

#[test]
fn reopen_finishes_a_pending_render_and_checkout_refuses_dirty_tree() {
    let (dir, repo) = fresh();
    let v0 = repo.store().root().unwrap();
    svc_repo::new(&repo).unwrap();
    repo.store().set_render_pending(true).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "garbage\n").unwrap();
    drop(repo);

    let repo = Repo::open(dir.path(), Repo::default_langs()).unwrap();
    assert_eq!(read(dir.path(), "src/lib.rs"), LIB, "pending render re-ran on open");
    assert!(!repo.store().render_pending().unwrap());

    std::fs::write(dir.path().join("src/lib.rs"), "fn hand_edit() {}\n").unwrap();
    assert!(svc_repo::checkout(&repo, v0).is_err());
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    svc_repo::checkout(&repo, v0).unwrap();
    assert_eq!(repo.store().root().unwrap(), v0);
}

#[test]
fn init_cannot_be_undone() {
    let (_dir, repo) = fresh();
    assert!(svc_repo::undo(&repo).is_err());
    let id = *repo.current().unwrap().entities.keys().next().unwrap();
    let blame = svc_repo::blame(&repo, id).unwrap();
    assert_eq!(blame.len(), 1);
    assert_eq!(blame[0].touch, Touch::Added);
}

#[test]
fn changeset_verbs_stamp_ops_and_report() {
    let (_dir, repo) = fresh();
    assert!(svc_repo::changeset_status(&repo).unwrap().is_none());
    let cs = svc_repo::changeset_begin(&repo, "agent run", Intent::Refactor, None, false).unwrap();
    assert!(cs.open && cs.ops.is_empty());
    assert!(svc_repo::changeset_begin(&repo, "again", Intent::Fix, None, false).is_err());
    svc_repo::describe(&repo, "by the agent").unwrap();
    let status = svc_repo::changeset_status(&repo).unwrap().unwrap();
    assert_eq!(status.id, cs.id);
    assert_eq!(status.ops.len(), 1);
    assert_eq!(svc_repo::changeset_end(&repo).unwrap(), Some(cs.id));
    assert!(svc_repo::changeset_status(&repo).unwrap().is_none());
    svc_repo::describe(&repo, "by a human").unwrap();
    let all = svc_repo::changesets(&repo).unwrap();
    assert_eq!(all.len(), 1);
    assert!(!all[0].open);
    assert_eq!(all[0].ops.len(), 1, "the human's op is not in the group");
    let forced = svc_repo::changeset_begin(&repo, "x", Intent::Fix, None, true).unwrap();
    assert_ne!(forced.id, cs.id);
}

#[test]
fn resolve_entity_by_name_qualified_name_and_id() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "struct A;\nimpl A { fn new() -> A { A } }\nstruct B;\nimpl B { fn new() -> B { B } }\n",
    )
    .unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    assert!(svc_repo::resolve_entity(&repo, "new").is_err(), "ambiguous");
    assert!(svc_repo::resolve_entity(&repo, "nope").is_err());
    let a = svc_repo::resolve_entity(&repo, "A").unwrap();
    assert_eq!(svc_repo::resolve_entity(&repo, &a.to_string()).unwrap(), a);
    let snap = repo.current().unwrap();
    let impl_names: Vec<_> = snap.entities.values().filter(|r| r.kind == svc_core::Kind::Impl).map(|r| r.name.clone()).collect();
    let by_impl = svc_repo::resolve_entity(&repo, &format!("{}::new", impl_names[0]));
    assert!(by_impl.is_ok(), "impl names: {impl_names:?}");
}
