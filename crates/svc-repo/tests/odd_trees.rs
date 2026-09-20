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

#[test]
fn a_store_still_importing_is_not_a_checkout_and_the_next_init_sweeps_it() {
    use svc_repo::repo::{STORE_FILE, STORE_FILE_IMPORTING};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    // What a SIGKILLed init leaves: the directory, a store under the importing name, no root.
    std::fs::create_dir_all(root.join(STORE_DIR)).unwrap();
    std::fs::write(root.join(STORE_DIR).join(STORE_FILE_IMPORTING), b"half").unwrap();

    assert!(Repo::find_root(root).is_none(), "not a checkout yet");
    let err = Repo::open(root, Repo::default_langs()).err().expect("open refuses");
    assert!(err.to_string().contains("no .svc"), "{err}");
    let repo = Repo::init(root, Repo::default_langs()).expect("init sweeps the leftover");
    assert!(!root.join(STORE_DIR).join(STORE_FILE_IMPORTING).exists());
    assert!(root.join(STORE_DIR).join(STORE_FILE).is_file());
    assert_eq!(repo.store_path(), root.join(STORE_DIR).join(STORE_FILE));
    assert!(repo.working_copy_clean().unwrap());
    // And a live store is never under the importing name.
    drop(repo);
    Repo::open(root, Repo::default_langs()).unwrap();
}

#[test]
fn a_file_name_that_is_not_utf8_is_refused_by_name() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    let weird = root.join(std::ffi::OsStr::from_bytes(b"weird-\xff-name.txt"));
    std::fs::write(&weird, "x").unwrap();

    let err = Repo::init(root, Repo::default_langs()).err().expect("refused");
    let msg = err.to_string();
    assert!(msg.contains("not UTF-8") && msg.contains("weird-") && msg.contains(".svcignore"), "{msg}");
    assert!(!root.join(STORE_DIR).exists(), "and nothing left behind");
    // Ignoring its directory is one of the two ways out.
    std::fs::create_dir_all(root.join("odd")).unwrap();
    std::fs::rename(&weird, root.join("odd").join(std::ffi::OsStr::from_bytes(b"weird-\xff-name.txt"))).unwrap();
    std::fs::write(root.join(".svcignore"), "odd\n").unwrap();
    Repo::init(root, Repo::default_langs()).expect("init with the directory ignored");
}
