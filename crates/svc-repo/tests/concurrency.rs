//! Concurrency semantics (ironmoon's done-criterion 4), stated and enforced:
//!
//! 1. **Session.** One `Repo` open is one exclusive store session. redb refuses a second
//!    handle on the file — from another process or this one — until the first is dropped, and
//!    `Repo::open` waits up to `SVC_LOCK_TIMEOUT_MS` for that, then fails with `store busy`.
//!    Every verb is therefore serialised and atomic with respect to every other verb.
//! 2. **Order.** The op log is the total order. `OpIx` is contiguous, and for one checkout
//!    entry *i*'s `after` view is entry *i+1*'s `before` view. N writers × M mutations leave
//!    exactly N·M entries, an intact chain, a working copy equal to the final snapshot, and a
//!    green O5 replay.
//! 3. **Checkouts.** A workspace's `root` moves only by ops issued from it; heads are shared.
//!    Two checkouts on one change: the one that did not write is *stale* and its mutations
//!    refuse until `workspace::update_stale`. Nothing is ever silently overwritten.

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use svc_core::{Op, OpIx};
use svc_repo::{Repo, workspace};

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

/// Generous: this VM runs six agents and their builds; a single redb open can take seconds.
const WAIT: Duration = Duration::from_secs(120);

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

fn open(root: &Path) -> Repo {
    Repo::open_with(root, Repo::default_langs(), WAIT).unwrap()
}

fn describe(repo: &Repo, msg: &str) {
    svc_repo::describe(repo, msg).unwrap();
}

fn rename_read(repo: &Repo, to: &str) {
    let id = svc_repo::resolve_entity(repo, "read").unwrap();
    repo.mutate(Op::Rename { id, new: to.into() }, None, |repo, cur| {
        let mut next = cur.clone();
        next.entities.get_mut(&id).unwrap().name = to.into();
        repo.amend(cur, next)
    })
    .unwrap();
}

#[test]
fn a_second_session_waits_then_reports_store_busy() {
    let (dir, repo) = fresh();
    let started = std::time::Instant::now();
    let err = Repo::open_with(dir.path(), Repo::default_langs(), Duration::from_millis(300))
        .err()
        .expect("second session must not open while the first is alive");
    assert!(err.to_string().contains("store busy"), "{err}");
    assert!(started.elapsed() >= Duration::from_millis(300), "it waited");
    drop(repo);
    open(dir.path());
}

#[test]
fn writers_serialise_into_one_contiguous_chained_op_log() {
    const WRITERS: usize = 6;
    const EACH: usize = 5;
    let (dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let ops_before = svc_repo::op_log(&repo).unwrap().len();
    drop(repo);

    let root = Arc::new(dir.path().to_path_buf());
    let gate = Arc::new(Barrier::new(WRITERS));
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (root, gate) = (root.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                for i in 0..EACH {
                    // Open → mutate → drop: the session is the critical section.
                    let repo = open(&root);
                    describe(&repo, &format!("writer {w} step {i}"));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let repo = open(dir.path());
    let ops = repo.store().ops(OpIx(0), false).unwrap();
    assert_eq!(ops.len(), ops_before + WRITERS * EACH, "every mutation is exactly one entry");
    for (k, (ix, _)) in ops.iter().enumerate() {
        assert_eq!(*ix, OpIx(k as u64), "OpIx is contiguous");
    }
    for pair in ops.windows(2) {
        assert_eq!(pair[0].1.after, pair[1].1.before, "entry i.after == entry i+1.before");
    }
    let last = &ops.last().unwrap().1;
    assert_eq!(last.after.root, repo.current().unwrap().id(), "root is the last op's after");
    assert!(repo.working_copy_clean().unwrap(), "disk equals the final snapshot");
    let report = svc_repo::replay(&repo).unwrap();
    assert!(report.ok(), "O5 replay diverged at {:?}", report.diverged_at);
}

#[test]
fn two_checkouts_on_two_changes_never_move_each_others_root() {
    let (a, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let a_change = repo.current_change().unwrap();
    let b = tempfile::tempdir().unwrap();
    workspace::add(&repo, "agent", b.path(), None).unwrap();
    drop(repo);
    // B gets its own change, as an agent's checkout would.
    let wb = open(b.path());
    svc_repo::new(&wb).unwrap();
    let b_change = wb.current_change().unwrap();
    drop(wb);
    assert_ne!(a_change, b_change);

    let (pa, pb) = (a.path().to_path_buf(), b.path().to_path_buf());
    let gate = Arc::new(Barrier::new(2));
    let ta = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            gate.wait();
            for i in 0..6 {
                let r = open(&pa);
                describe(&r, &format!("a{i}"));
            }
        })
    };
    let tb = std::thread::spawn(move || {
        gate.wait();
        for i in 0..6 {
            let r = open(&pb);
            if i == 2 {
                rename_read(&r, "read_file");
            } else {
                describe(&r, &format!("b{i}"));
            }
        }
    });
    ta.join().unwrap();
    tb.join().unwrap();

    let wa = open(a.path());
    let cur = wa.current().unwrap();
    assert_eq!(cur.change, a_change, "A still on its own change");
    assert_eq!(cur.message, "a5");
    assert!(!wa.is_stale().unwrap());
    assert!(wa.working_copy_clean().unwrap());
    assert_eq!(std::fs::read_to_string(a.path().join("src/lib.rs")).unwrap(), LIB);
    let rows = workspace::list(&wa).unwrap();
    assert!(rows.iter().all(|r| !r.stale), "{rows:?}");
    // Every op is attributed to the checkout that issued it.
    let store = wa.store();
    let all = store.ops(OpIx(0), false).unwrap();
    let mine = wa.redb().own_ops(OpIx(0), false).unwrap();
    assert!(mine.len() >= 6 && mine.len() < all.len(), "{} of {}", mine.len(), all.len());
    for (_, e) in &mine {
        assert_ne!(store.get_snapshot(e.after.root).unwrap().change, b_change, "A never wrote B's change");
    }
    assert!(all.iter().all(|(ix, _)| wa.redb().op_workspace(*ix).unwrap().is_some()));
    drop(wa);

    let wb = open(b.path());
    let cur = wb.current().unwrap();
    assert_eq!(cur.change, b_change);
    assert_eq!(cur.message, "b5");
    assert!(!wb.is_stale().unwrap());
    assert!(std::fs::read_to_string(b.path().join("src/lib.rs")).unwrap().contains("fn read_file("));
    assert_eq!(all.len(), 1 + 1 + 1 + 12, "init, new, new, 12 mutations");
}

#[test]
fn same_change_in_two_checkouts_makes_the_loser_stale_until_updated() {
    let (a, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    let change = repo.current_change().unwrap();
    let b = tempfile::tempdir().unwrap();
    workspace::add(&repo, "twin", b.path(), None).unwrap();
    drop(repo);

    // B amends the shared change; A's root is now behind the head.
    let wb = open(b.path());
    rename_read(&wb, "read_file");
    drop(wb);

    let wa = open(a.path());
    assert!(wa.is_stale().unwrap());
    let rows = workspace::list(&wa).unwrap();
    assert!(rows[0].stale && !rows[1].stale, "{rows:?}");
    let err = svc_repo::describe(&wa, "from a").err().expect("stale checkout refuses");
    assert!(err.to_string().contains("behind change"), "{err}");
    // Reading still works, and the working copy is untouched (still the old text).
    assert_eq!(std::fs::read_to_string(a.path().join("src/lib.rs")).unwrap(), LIB);
    assert_eq!(svc_repo::status(&wa).unwrap().change, change);

    // Hand edits on a stale checkout are not absorbed into a fork either.
    std::fs::write(a.path().join("src/lib.rs"), LIB.replace("Config", "Conf")).unwrap();
    assert!(svc_repo::describe(&wa, "x").is_err());
    assert!(workspace::update_stale(&wa).is_err(), "won't discard hand edits");
    std::fs::write(a.path().join("src/lib.rs"), LIB).unwrap();

    let out = workspace::update_stale(&wa).unwrap();
    assert!(!out.stale && out.snapshot == Some(wa.store().head(change).unwrap()));
    assert!(!wa.is_stale().unwrap());
    assert!(std::fs::read_to_string(a.path().join("src/lib.rs")).unwrap().contains("fn read_file("));
    describe(&wa, "from a"); // works again
    drop(wa);

    // …and now B is the stale one, symmetrically.
    let wb = open(b.path());
    assert!(wb.is_stale().unwrap());
    workspace::update_stale(&wb).unwrap();
    assert_eq!(wb.current().unwrap().message, "from a");
}
