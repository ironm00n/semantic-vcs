//! Named checkouts (`svc workspace add|list|forget`): two directories, one store, each with
//! its own `root`; snapshots, heads and the op log shared. redb is single-process, so every
//! `Repo` here is dropped before the next one opens the same store — that is the contract.

use std::path::Path;

use svc_core::Op;
use svc_repo::workspace::{self, DEFAULT_WORKSPACE, POINTER_FILE};
use svc_repo::{Repo, WorkspacePointer};

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(dir.path().join(".svcignore"), "vendor\n").unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn rename_read(repo: &Repo) {
    let id = svc_repo::resolve_entity(repo, "read").unwrap();
    repo.mutate(Op::Rename { id, new: "read_file".into() }, None, |repo, cur| {
        let mut next = cur.clone();
        next.entities.get_mut(&id).unwrap().name = "read_file".into();
        repo.amend(cur, next)
    })
    .unwrap();
}

#[test]
fn add_renders_the_snapshot_and_discover_finds_the_right_checkout() {
    let (a, repo) = fresh();
    let first = repo.current_change().unwrap();
    svc_repo::new(&repo).unwrap();
    let second = repo.current_change().unwrap();
    let head = repo.current().unwrap().id();

    let b = tempfile::tempdir().unwrap();
    let b_dir = b.path().join("agent");
    let out = workspace::add(&repo, "agent", &b_dir, None).unwrap();
    assert_eq!(out.name, "agent");
    assert_eq!(out.change, Some(second));
    assert_eq!(out.snapshot, Some(head));
    assert!(!out.current);
    assert_eq!(read(&b_dir, "src/lib.rs"), LIB, "rendered byte for byte");
    assert_eq!(read(&b_dir, ".svcignore"), "vendor\n", "ignore rules travel");
    let ptr = WorkspacePointer::read(&b_dir).unwrap().unwrap();
    assert_eq!(ptr.name, "agent");
    assert_eq!(ptr.store, repo.store_path());
    assert!(!b_dir.join(".svc").exists(), "no second store");

    // An older change can be checked out elsewhere.
    let c_dir = b.path().join("old");
    let old = workspace::add(&repo, "old", &c_dir, Some(first)).unwrap();
    assert_eq!(old.change, Some(first));

    let rows = workspace::list(&repo).unwrap();
    let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, [DEFAULT_WORKSPACE, "agent", "old"]);
    assert!(rows[0].current && !rows[1].current);
    assert_eq!(rows[0].path, a.path().canonicalize().unwrap());
    assert_eq!(rows[0].snapshot, Some(head));
    assert_eq!(rows[2].change, Some(first));

    assert!(workspace::add(&repo, "agent", &b.path().join("x"), None).is_err(), "dup name");
    assert!(workspace::add(&repo, "default", &b.path().join("y"), None).is_err(), "reserved");
    assert!(workspace::add(&repo, "again", &b_dir, None).is_err(), "non-empty dir");
    drop(repo);

    // Discover from inside the named checkout: its root, its name, the shared store.
    assert_eq!(Repo::find_root(&b_dir.join("src")).unwrap(), b_dir);
    let wb = Repo::discover(&b_dir.join("src"), Repo::default_langs()).unwrap();
    assert_eq!(wb.root_dir(), b_dir);
    assert_eq!(wb.workspace(), Some("agent"));
    assert_eq!(wb.current().unwrap().id(), head);
    assert!(wb.working_copy_clean().unwrap());
    assert!(Repo::init(&b_dir, Repo::default_langs()).is_err(), "no init inside a workspace");
    assert!(workspace::list(&wb).unwrap()[1].current);
    assert!(workspace::forget(&wb, "agent").is_err(), "not from inside itself");
    drop(wb);

    let wa = Repo::discover(&a.path().join("src"), Repo::default_langs()).unwrap();
    assert_eq!(wa.workspace(), None);
    assert_eq!(wa.current().unwrap().id(), head);
}

#[test]
fn ops_in_one_checkout_share_heads_and_log_but_not_the_working_copy() {
    let (a, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let a_snap = repo.current().unwrap().id();
    let ops_before = svc_repo::op_log(&repo).unwrap().len();
    let b = tempfile::tempdir().unwrap();
    workspace::add(&repo, "agent", b.path(), None).unwrap();
    drop(repo);

    let wb = Repo::discover(b.path(), Repo::default_langs()).unwrap();
    svc_repo::new(&wb).unwrap();
    let b_change = wb.current_change().unwrap();
    rename_read(&wb);
    let b_snap = wb.current().unwrap().id();
    assert!(read(b.path(), "src/lib.rs").contains("fn read_file("));
    assert_eq!(read(a.path(), "src/lib.rs"), LIB, "the other checkout is untouched");
    drop(wb);

    let wa = Repo::open(a.path(), Repo::default_langs()).unwrap();
    assert_eq!(wa.current().unwrap().id(), a_snap, "default root did not move");
    assert!(wa.working_copy_clean().unwrap());
    assert_eq!(svc_repo::op_log(&wa).unwrap().len(), ops_before + 2, "op log is shared");
    assert_eq!(wa.store().head(b_change).unwrap(), b_snap, "heads are shared");
    let heads = svc_repo::heads(&wa).unwrap();
    assert!(heads.iter().any(|h| h.change == b_change && !h.current));
    let rows = workspace::list(&wa).unwrap();
    assert_eq!(rows[1].change, Some(b_change));
    assert_eq!(rows[1].snapshot, Some(b_snap));

    // Forgetting drops the row and the pointer; files stay; discover no longer resolves.
    let gone = workspace::forget(&wa, "agent").unwrap();
    assert_eq!(gone.change, Some(b_change));
    assert!(!b.path().join(POINTER_FILE).exists());
    assert!(b.path().join("src/lib.rs").exists());
    assert_eq!(workspace::list(&wa).unwrap().len(), 1);
    assert!(workspace::forget(&wa, "agent").is_err());
    drop(wa);
    assert!(Repo::discover(b.path(), Repo::default_langs()).is_err());
}

#[test]
fn a_crashed_render_in_a_named_checkout_finishes_on_open() {
    let (_a, repo) = fresh();
    let b = tempfile::tempdir().unwrap();
    workspace::add(&repo, "agent", b.path(), None).unwrap();
    drop(repo);

    let wb = Repo::discover(b.path(), Repo::default_langs()).unwrap();
    rename_read(&wb);
    // Simulate a crash between commit and render: pending flag on, stale bytes on disk.
    std::fs::write(b.path().join("src/lib.rs"), LIB).unwrap();
    wb.store().set_render_pending(true).unwrap();
    drop(wb);

    let wb = Repo::discover(b.path(), Repo::default_langs()).unwrap();
    assert!(read(b.path(), "src/lib.rs").contains("fn read_file("), "render finished on open");
    assert!(!wb.store().render_pending().unwrap());
}

/// DEBATE §15.3: `undo` in a checkout walks that checkout's own ops. B's undo reverts B's
/// rename and leaves A's alone even though A's op is newer in the shared log; A's undo
/// then reverts A's. Neither ever moves the other's root.
#[test]
fn undo_is_per_checkout() {
    let (a, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let b = tempfile::tempdir().unwrap();
    workspace::add(&repo, "agent", b.path(), None).unwrap();
    drop(repo);
    let wb = Repo::discover(b.path(), Repo::default_langs()).unwrap();
    svc_repo::new(&wb).unwrap(); // B on its own change
    let b_change = wb.current_change().unwrap();
    rename_read(&wb); // B: read → read_file
    drop(wb);
    let wa = Repo::discover(a.path(), Repo::default_langs()).unwrap();
    let a_change = wa.current_change().unwrap();
    let id = svc_repo::resolve_entity(&wa, "main").unwrap();
    wa.mutate(Op::Rename { id, new: "entry".into() }, None, |repo, cur| {
        let mut next = cur.clone();
        next.entities.get_mut(&id).unwrap().name = "entry".into();
        repo.amend(cur, next)
    })
    .unwrap(); // A: main → entry, the newest op in the shared log
    drop(wa);

    let wb = Repo::discover(b.path(), Repo::default_langs()).unwrap();
    svc_repo::undo(&wb).unwrap();
    assert_eq!(wb.current_change().unwrap(), b_change, "B stays on its own change (a global undo would land it on A's)");
    assert!(read(b.path(), "src/lib.rs").contains("fn read("), "B undid its own rename");
    assert!(read(a.path(), "src/lib.rs").contains("fn entry("), "A's checkout untouched by B's undo");
    drop(wb);

    let wa = Repo::discover(a.path(), Repo::default_langs()).unwrap();
    svc_repo::undo(&wa).unwrap();
    assert_eq!(wa.current_change().unwrap(), a_change);
    assert!(read(a.path(), "src/lib.rs").contains("fn main("), "A undid its own rename");
    assert!(read(b.path(), "src/lib.rs").contains("fn read("), "B still at its undone state");
}
