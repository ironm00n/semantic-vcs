//! O5: replaying the op log on a fresh store reproduces every snapshot's content.

use svc_core::engine::{add_def, edit_def, rename};
use svc_core::{EntityId, Intent, Op};
use svc_repo::Repo;

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn helper_base() {}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

fn rename_op(repo: &Repo, name: &str, new: &str) {
    let id = svc_repo::resolve_entity(repo, name).unwrap();
    repo.mutate(Op::Rename { id, new: new.into() }, None, |repo, cur| repo.amend(cur, rename(cur, id, new)?))
        .unwrap();
}

fn edit(repo: &Repo, name: &str, definition: &str) {
    let id = svc_repo::resolve_entity(repo, name).unwrap();
    repo.mutate(
        Op::EditDef { id, definition: definition.into(), intent: Intent::Feature },
        None,
        |repo, cur| {
            let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
            repo.amend(cur, next)
        },
    )
    .unwrap();
}

fn add(repo: &Repo, definition: &str) {
    let id = EntityId::new();
    let ordinal = repo.current().unwrap().entities.len() as u32;
    repo.mutate(
        Op::AddDef { id, parent: None, ordinal, definition: definition.into(), intent: Intent::Feature },
        None,
        |repo, cur| {
            let next = add_def(repo.store(), repo.langs(), cur, id, None, ordinal, definition.as_bytes(), Intent::Feature)?;
            repo.amend(cur, next)
        },
    )
    .unwrap();
}

#[test]
fn o5_replay_reproduces_every_snapshot() {
    let (dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    rename_op(&repo, "read", "read_file");
    svc_repo::describe(&repo, "rename read").unwrap();
    edit(&repo, "main", "fn main() {\n    let _ = read_file(\"y\");\n}\n");
    // A hand edit, absorbed by the next op.
    std::fs::write(
        dir.path().join("src/lib.rs"),
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap().replace("fn helper_base() {}", "fn helper_base() { let _ = 0; }"),
    )
    .unwrap();
    svc_repo::status(&repo).unwrap();
    svc_repo::new(&repo).unwrap();
    svc_repo::branch(&repo, "a").unwrap();
    add(&repo, "fn from_a() {}\n");
    svc_repo::branch(&repo, "b").unwrap();
    add(&repo, "fn from_b() {}\n");
    svc_repo::merge(&repo, "a").unwrap();
    svc_repo::undo(&repo).unwrap();
    svc_repo::undo(&repo).unwrap();

    let report = svc_repo::replay(&repo).unwrap();
    assert!(report.ops >= 12, "{report:?}");
    assert!(report.ok(), "replay diverged: {report:?}");
}
