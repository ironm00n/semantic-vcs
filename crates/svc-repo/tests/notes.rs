//! Review, mail, and claims share `Op::Note` (DEBATE §19).

use svc_core::{NoteKind, NoteTo, Op};
use svc_repo::Repo;

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

#[test]
fn review_approve_hangs_on_the_changeset_and_does_not_move_the_snapshot() {
    let (_dir, repo) = fresh();
    svc_repo::changeset_begin(&repo, "run", svc_core::Intent::Refactor, None, false).unwrap();
    svc_repo::changeset_end(&repo).unwrap();
    let before = repo.current().unwrap().id();
    svc_repo::review(&repo, "run", NoteKind::Approve, "").unwrap();
    assert_eq!(repo.current().unwrap().id(), before, "a note does not rewrite the tree");
    let shown = svc_repo::changeset_show(&repo, "run").unwrap();
    assert_eq!(shown.reviews.len(), 1);
    match &shown.reviews[0].op {
        Op::Note {
            to: NoteTo::Changeset(id),
            kind: NoteKind::Approve,
            ..
        } => assert_eq!(*id, shown.id),
        other => panic!("{other:?}"),
    }
}

#[test]
fn mail_to_all_is_unread_until_read() {
    let (_dir, repo) = fresh();
    svc_repo::mail(&repo, "@all", "pushing main", None).unwrap();
    let box_ = svc_repo::inbox(&repo).unwrap();
    assert_eq!(box_.unread.len(), 1);
    assert!(matches!(
        &box_.unread[0].op,
        Op::Note {
            to: NoteTo::All,
            kind: NoteKind::Note,
            text
        } if text == "pushing main"
    ));
    let ix = box_.unread[0].ix.0;
    svc_repo::mail_read(&repo, ix).unwrap();
    assert!(svc_repo::inbox(&repo).unwrap().unread.is_empty());
}

#[test]
fn claim_then_release_clears_status() {
    let (_dir, repo) = fresh();
    let id = svc_repo::resolve_entity(&repo, "read").unwrap();
    svc_repo::claim(&repo, &["read".into()]).unwrap();
    let claims = svc_repo::active_claims(&repo).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].entity, id);
    assert_eq!(claims[0].by, "default");
    svc_repo::release(&repo, &["read".into()]).unwrap();
    assert!(svc_repo::active_claims(&repo).unwrap().is_empty());
}

#[test]
fn note_about_an_entity_is_on_show_def() {
    let (_dir, repo) = fresh();
    let id = svc_repo::resolve_entity(&repo, "read").unwrap();
    svc_repo::mail(&repo, "default", "look at read", Some("read")).unwrap();
    let notes = svc_repo::notes_about_entity(&repo, id).unwrap();
    assert_eq!(notes.len(), 1);
    match &notes[0].op {
        Op::Note {
            to: NoteTo::Entity(eid),
            text,
            ..
        } => {
            assert_eq!(*eid, id);
            assert_eq!(text, "look at read");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn review_note_survives_history_export_import() {
    let (dir, repo) = fresh();
    svc_repo::changeset_begin(&repo, "run", svc_core::Intent::Refactor, None, false).unwrap();
    svc_repo::changeset_end(&repo).unwrap();
    svc_repo::review(&repo, "run", NoteKind::RequestChanges, "split this").unwrap();
    let bundle = svc_repo::bundle::export(&repo, svc_core::OpIx(0)).unwrap();
    assert!(
        bundle.entries.iter().any(|e| matches!(
            e.entry.op,
            Op::Note {
                kind: NoteKind::RequestChanges,
                ..
            }
        )),
        "the note travelled in the bundle"
    );

    let dest = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dest.path().join("src")).unwrap();
    std::fs::copy(dir.path().join("src/lib.rs"), dest.path().join("src/lib.rs")).unwrap();
    std::fs::copy(dir.path().join("Cargo.toml"), dest.path().join("Cargo.toml")).unwrap();
    let fresh = Repo::init(dest.path(), Repo::default_langs()).unwrap();
    // Import needs the same base tree as the bundle; init already ingested it.
    // Re-export from op 1 to skip init, matching how history import is used.
    let from_one = svc_repo::bundle::export(&repo, svc_core::OpIx(1)).unwrap();
    let report = svc_repo::bundle::import(&fresh, &from_one).unwrap();
    assert!(report.diverged_at.is_none(), "{report:?}");
    let shown = svc_repo::changeset_show(&fresh, "run").unwrap();
    assert_eq!(shown.reviews.len(), 1, "{shown:?}");
}
