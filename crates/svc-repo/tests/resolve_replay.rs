//! `svc resolve` is an op of its own. It used to be logged as `Describe("")`, so the
//! log did not say a conflict had been resolved and replay diverged right after it.
use svc_core::engine::edit_def;
use svc_core::{Conflict, Intent, Op, Take};
use svc_repo::Repo;

const LIB: &str = "fn limit() -> u32 {\n    10\n}\n\nfn main() {\n    let _ = limit();\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    (dir, repo)
}

fn edit(repo: &Repo, name: &str, definition: &str) {
    let id = svc_repo::resolve_entity(repo, name).unwrap();
    repo.mutate(
        Op::EditDef {
            id,
            definition: definition.into(),
            intent: Intent::Fix,
        },
        None,
        |repo, cur| {
            let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
            repo.amend(cur, next)
        },
    )
    .unwrap();
}

#[test]
fn resolve_is_logged_as_itself_and_replays() {
    let (dir, repo) = fresh();
    svc_repo::new(&repo).unwrap();
    edit(&repo, "limit", "fn limit() -> u32 {\n    5\n}\n");
    let a = repo.current_change().unwrap();
    svc_repo::branch(&repo, "b").unwrap();
    edit(&repo, "limit", "fn limit() -> u32 {\n    20\n}\n");
    let m = svc_repo::merge(&repo, &a.to_string()).unwrap();
    assert_eq!(m.conflicts.len(), 1, "{:?}", m.conflicts);
    assert!(matches!(
        repo.current().unwrap().conflicts[0],
        Conflict::Content { .. }
    ));

    // Side A is the current change (the merge's first parent); B is the change merged in.
    svc_repo::resolve(&repo, 0, Take::B).unwrap();
    let cur = repo.current().unwrap();
    assert!(cur.conflicts.is_empty());
    let rendered = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        rendered.contains("    5\n"),
        "took B (the merged-in change):\n{rendered}"
    );

    let last = svc_repo::op_log(&repo).unwrap().into_iter().next().unwrap();
    assert!(
        matches!(
            last.op,
            Op::Resolve {
                conflict: 0,
                take: Take::B
            }
        ),
        "last op is the resolution, not a describe: {:?}",
        last.op
    );

    let report = svc_repo::replay(&repo).unwrap();
    assert!(report.ok(), "replay diverged: {report:?}");
}
