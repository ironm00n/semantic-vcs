//! Trees a judge's checkout might contain and a store must not choke on: symlinks (dangling,
//! to a file, to a directory), a FIFO, an empty file, a large opaque file — and an import
//! that fails must leave no `.svc/` behind.

use std::os::unix::fs::{PermissionsExt, symlink};

use svc_repo::Repo;
use svc_repo::repo::STORE_DIR;

const LIB: &str = "fn read(path: &str) -> String {\n    path.to_string()\n}\n";

#[test]
fn init_tracks_regular_files_only_and_skips_links_and_fifos() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    std::fs::write(root.join("sub/m.rs"), "fn b() {}\n").unwrap();
    std::fs::write(root.join("empty.txt"), "").unwrap();
    std::fs::write(root.join("big.bin"), vec![7u8; 5_000_000]).unwrap();
    symlink("missing", root.join("dangling")).unwrap();
    symlink("src/lib.rs", root.join("lib_link.rs")).unwrap();
    symlink("../src", root.join("sub/link_to_src")).unwrap();
    // mkfifo(3) through libc is not on std's menu; the shell has it.
    assert!(std::process::Command::new("mkfifo").arg(root.join("pipe.fifo")).status().unwrap().success());

    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    let tracked = repo.tracked_files().unwrap();
    let names: Vec<&str> = tracked.keys().map(|p| p.as_str()).collect();
    assert_eq!(names, ["big.bin", "empty.txt", "src/lib.rs", "sub/m.rs"], "{names:?}");
    assert!(repo.working_copy_clean().unwrap());
    assert_eq!(svc_repo::status(&repo).unwrap().entities, 2);
}

#[test]
fn a_failed_init_leaves_no_store_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    std::fs::write(root.join("secret"), "x").unwrap();
    std::fs::set_permissions(root.join("secret"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let err = Repo::init(root, Repo::default_langs()).err().expect("an unreadable file fails the import");
    assert!(err.to_string().contains("ermission"), "{err}");
    assert!(!root.join(STORE_DIR).exists(), "no half-made .svc/");
    std::fs::set_permissions(root.join("secret"), std::fs::Permissions::from_mode(0o644)).unwrap();
    Repo::init(root, Repo::default_langs()).expect("init works once the tree is readable");
}
