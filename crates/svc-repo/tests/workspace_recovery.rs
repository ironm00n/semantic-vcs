//! `workspace add` interrupted between its pointer and its row, and a pointer carried to a
//! directory that is not the registered checkout.

use svc_repo::workspace::{self, POINTER_FILE};
use svc_repo::{Repo, WorkspacePointer};

const LIB: &str = "fn read(path: &str) -> String {\n    path.to_string()\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

#[test]
fn an_add_killed_after_its_pointer_is_finished_by_running_it_again() {
    let (_a, repo) = fresh();
    let b = tempfile::tempdir().unwrap();
    let dir = b.path().join("w1");
    std::fs::create_dir_all(&dir).unwrap();
    // What a kill between the pointer and the row leaves.
    WorkspacePointer { store: repo.store_path().to_path_buf(), name: "w1".into() }
        .write(&dir)
        .unwrap();
    let err = Repo::open(&dir, Repo::default_langs()).err().expect("not a checkout yet");
    assert!(err.to_string().contains("workspace \"w1\""), "{err}");

    let out = workspace::add(&repo, "w1", &dir, None).expect("the same add finishes the job");
    assert_eq!(out.name, "w1");
    assert!(dir.join("src/lib.rs").is_file(), "rendered");
    let w1 = Repo::open(&dir, Repo::default_langs()).unwrap();
    assert!(w1.working_copy_clean().unwrap());

    // A directory holding a pointer to some other workspace is still refused.
    let other = b.path().join("w2");
    std::fs::create_dir_all(&other).unwrap();
    WorkspacePointer { store: repo.store_path().to_path_buf(), name: "w1".into() }
        .write(&other)
        .unwrap();
    let err = workspace::add(&repo, "w2", &other, None).err().expect("refused");
    assert!(err.to_string().contains("another store or workspace"), "{err}");
}

#[test]
fn a_pointer_in_a_directory_that_is_not_the_registered_checkout_is_refused() {
    let (_a, repo) = fresh();
    let b = tempfile::tempdir().unwrap();
    let real = b.path().join("real");
    workspace::add(&repo, "w1", &real, None).unwrap();
    let elsewhere = b.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::copy(real.join(POINTER_FILE), elsewhere.join(POINTER_FILE)).unwrap();

    let err = Repo::open(&elsewhere, Repo::default_langs()).err().expect("refused");
    let msg = err.to_string();
    assert!(msg.contains("registered at") && msg.contains("real"), "{msg}");
    // The real checkout is untouched and still opens.
    Repo::open(&real, Repo::default_langs()).unwrap();
}
