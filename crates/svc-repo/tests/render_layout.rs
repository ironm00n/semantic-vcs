use std::fs;
use std::path::Path;

use svc_core::Store;
use svc_repo::Repo;

fn put(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn opaque_files_survive_history_and_a_store_only_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, "assets/data.bin", b"\0\xff\x80\r\n");
    put(root, "assets/empty", b"");
    put(root, "obsolete.txt", b"remove me");
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    let before = repo.tracked_files().unwrap();
    put(root, "assets/data.bin", b"\xff\0\x01\x02");
    put(root, "assets/new-empty", b"");
    fs::remove_file(root.join("obsolete.txt")).unwrap();
    let after = repo.tracked_files().unwrap();
    assert!(svc_repo::status(&repo).unwrap().absorbed);
    let ix = svc_repo::op_log(&repo).unwrap()[0].ix;
    svc_repo::undo(&repo).unwrap();
    assert_eq!(repo.tracked_files().unwrap(), before);
    assert!(repo.working_copy_clean().unwrap());
    svc_repo::op_restore(&repo, ix).unwrap();
    assert_eq!(repo.tracked_files().unwrap(), after);
    assert!(repo.working_copy_clean().unwrap());
    assert!(svc_repo::replay(&repo).unwrap().diverged_at.is_none());
    let checkout = tempfile::tempdir().unwrap();
    svc_repo::workspace::add(&repo, "copy", checkout.path(), None).unwrap();
    let copy = Repo::open(checkout.path(), Repo::default_langs()).unwrap();
    assert_eq!(copy.tracked_files().unwrap(), after);
}

#[test]
fn undo_and_restore_handle_both_file_directory_transitions() {
    for start_with_file in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (before, after) = if start_with_file {
            ("payload", "payload/nested/data.bin")
        } else {
            ("payload/nested/data.bin", "payload")
        };
        put(root, before, b"before\0\xff");
        let repo = Repo::init(root, Repo::default_langs()).unwrap();
        fs::remove_file(root.join(before)).unwrap();
        if !start_with_file {
            fs::remove_dir(root.join("payload/nested")).unwrap();
            fs::remove_dir(root.join("payload")).unwrap();
        }
        put(root, after, b"after\0\xfe");
        assert!(svc_repo::status(&repo).unwrap().absorbed);
        let ix = svc_repo::op_log(&repo).unwrap()[0].ix;
        svc_repo::undo(&repo).unwrap();
        assert_eq!(fs::read(root.join(before)).unwrap(), b"before\0\xff");
        assert!(repo.working_copy_clean().unwrap());
        svc_repo::op_restore(&repo, ix).unwrap();
        assert_eq!(fs::read(root.join(after)).unwrap(), b"after\0\xfe");
        assert!(repo.working_copy_clean().unwrap());
        assert!(svc_repo::replay(&repo).unwrap().diverged_at.is_none());
    }
}

#[test]
fn render_preserves_ignored_temporary_looking_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, ".svcignore", b"data.bin.svc-tmp\n");
    put(root, "data.bin", b"before");
    put(root, "data.bin.svc-tmp", b"untracked");
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    put(root, "data.bin", b"after");
    svc_repo::status(&repo).unwrap();
    svc_repo::undo(&repo).unwrap();
    assert_eq!(fs::read(root.join("data.bin")).unwrap(), b"before");
    assert_eq!(
        fs::read(root.join("data.bin.svc-tmp")).unwrap(),
        b"untracked"
    );
    assert!(repo.working_copy_clean().unwrap());
}

#[test]
fn ignored_obstruction_is_named_and_pending_render_recovers_after_it_is_moved() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, ".svcignore", b"kept.cache\n");
    put(root, "payload", b"original");
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    fs::remove_file(root.join("payload")).unwrap();
    put(root, "payload/nested/data.bin", b"tracked");
    put(root, "payload/nested/kept.cache", b"untracked");
    svc_repo::status(&repo).unwrap();
    let error = svc_repo::undo(&repo).unwrap_err().to_string();
    assert!(error.contains("payload/nested/kept.cache"), "{error}");
    assert_eq!(
        fs::read(root.join("payload/nested/kept.cache")).unwrap(),
        b"untracked"
    );
    assert_eq!(
        fs::read(root.join("payload/nested/data.bin")).unwrap(),
        b"tracked"
    );
    assert!(repo.redb().render_pending().unwrap());
    drop(repo);
    let saved = tempfile::tempdir().unwrap();
    fs::rename(
        root.join("payload/nested/kept.cache"),
        saved.path().join("kept.cache"),
    )
    .unwrap();
    let repo = Repo::open(root, Repo::default_langs()).unwrap();
    assert_eq!(fs::read(root.join("payload")).unwrap(), b"original");
    assert_eq!(
        fs::read(saved.path().join("kept.cache")).unwrap(),
        b"untracked"
    );
    assert!(!repo.redb().render_pending().unwrap());
    assert!(repo.working_copy_clean().unwrap());
}

#[test]
fn empty_directories_do_not_block_restoring_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, "payload", b"original");
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    fs::remove_file(root.join("payload")).unwrap();
    fs::create_dir_all(root.join("payload/empty/nested")).unwrap();
    svc_repo::status(&repo).unwrap();
    svc_repo::undo(&repo).unwrap();
    assert_eq!(fs::read(root.join("payload")).unwrap(), b"original");
    assert!(repo.working_copy_clean().unwrap());
}

#[test]
fn ignored_files_cannot_be_overwritten_or_removed_to_make_a_parent_directory() {
    for before in ["payload", "payload/nested/data.bin"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, ".svcignore", b"");
        put(root, before, b"original");
        let repo = Repo::init(root, Repo::default_langs()).unwrap();
        fs::remove_file(root.join(before)).unwrap();
        if before != "payload" {
            fs::remove_dir(root.join("payload/nested")).unwrap();
            fs::remove_dir(root.join("payload")).unwrap();
        }
        put(root, "payload", b"untracked");
        put(root, ".svcignore", b"payload\n");
        svc_repo::status(&repo).unwrap();
        let error = svc_repo::undo(&repo).unwrap_err().to_string();
        assert!(
            error.contains("untracked path obstructs render:"),
            "{error}"
        );
        assert!(error.contains("payload"), "{error}");
        assert_eq!(fs::read(root.join("payload")).unwrap(), b"untracked");
        assert_eq!(fs::read(root.join(".svcignore")).unwrap(), b"payload\n");
    }
}

#[cfg(unix)]
#[test]
fn rewriting_a_file_preserves_its_existing_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, "run.sh", b"before");
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o751)).unwrap();
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    put(root, "run.sh", b"after");
    svc_repo::status(&repo).unwrap();
    svc_repo::undo(&repo).unwrap();
    assert_eq!(
        fs::metadata(root.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
}

#[test]
fn rendering_an_old_ignore_file_does_not_delete_previously_ignored_data() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, ".svcignore", b"");
    put(root, "data.bin", b"before");
    let repo = Repo::init(root, Repo::default_langs()).unwrap();
    put(root, ".svcignore", b"kept.cache\n");
    put(root, "kept.cache", b"untracked");
    svc_repo::status(&repo).unwrap();
    svc_repo::undo(&repo).unwrap();
    assert_eq!(fs::read(root.join("kept.cache")).unwrap(), b"untracked");
}
