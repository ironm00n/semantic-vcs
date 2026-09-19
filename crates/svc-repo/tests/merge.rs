//! Acceptance tests for `svc merge` (DEBATE §11): the verbs are svc-repo's, the algorithm is
//! `svc_core::engine::merge`. Fixtures go through the real ops so the merged snapshot is
//! something the §5.4 post-condition can render and re-parse.

use svc_core::engine::{add_def, delete, edit_def, rename};
use svc_core::{Conflict, EntityId, Intent, Op};
use svc_repo::{Repo, Take};

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn helper_base() {}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

fn fresh() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    let repo = Repo::init(dir.path(), Repo::default_langs()).unwrap();
    svc_repo::new(&repo).unwrap();
    (dir, repo)
}

fn entity(repo: &Repo, name: &str) -> EntityId {
    svc_repo::resolve_entity(repo, name).unwrap()
}

fn rename_op(repo: &Repo, id: EntityId, new: &str) {
    repo.mutate(Op::Rename { id, new: new.into() }, None, |repo, cur| {
        repo.amend(cur, rename(cur, id, new)?)
    })
    .unwrap();
}

fn edit(repo: &Repo, id: EntityId, definition: &str) {
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

fn add(repo: &Repo, definition: &str) -> EntityId {
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
    id
}

fn text(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap()
}

#[test]
fn demo_line_5_rename_on_a_edit_on_b_merges_clean() {
    let (dir, repo) = fresh();
    let read = entity(&repo, "read");
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    rename_op(&repo, read, "read_file");
    svc_repo::branch(&repo, "b").unwrap();
    assert_eq!(repo.current().unwrap().entities[&read].name, "read", "b is a sibling of a");
    edit(&repo, main, "fn main() {\n    let _ = read(\"x\");\n    let _ = read(\"y\");\n}\n");

    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    assert_eq!(snap.change, out.change);
    assert_eq!(snap.parents.len(), 2);
    assert_eq!(snap.entities[&read].name, "read_file");
    let t = text(&dir);
    assert!(t.contains("fn read_file(path"), "{t}");
    assert!(t.contains("read_file(\"y\")"), "B's new call renders A's name: {t}");
    assert!(!t.contains("read(\""), "{t}");
    assert!(repo.working_copy_clean().unwrap());
    let log = svc_repo::log(&repo, None).unwrap();
    assert!(matches!(log[0].op, Op::Merge { .. }));
}

#[test]
fn rename_rename_is_an_attr_conflict_resolved_by_take() {
    let (_dir, repo) = fresh();
    let read = entity(&repo, "read");
    svc_repo::branch(&repo, "a").unwrap();
    rename_op(&repo, read, "load");
    svc_repo::branch(&repo, "b").unwrap();
    rename_op(&repo, read, "fetch");
    let out = svc_repo::merge(&repo, "a").unwrap();
    let attrs: Vec<_> = out.conflicts.iter().filter(|c| matches!(c.conflict, Conflict::Attr { .. })).collect();
    assert_eq!(attrs.len(), 1, "{:?}", out.conflicts);
    assert_eq!(out.conflicts.len(), 1, "no other conflicts: {:?}", out.conflicts);
    let n = attrs[0].n;

    let resolved = svc_repo::resolve(&repo, n, Take::B).unwrap();
    assert!(resolved.conflicts.is_empty(), "{:?}", resolved.conflicts);
    assert_eq!(repo.current().unwrap().entities[&read].name, "load");
    assert_eq!(repo.current().unwrap().change, out.change, "resolve amends the merge change");
    assert!(svc_repo::resolve(&repo, n, Take::A).is_err());
}

#[test]
fn delete_edit_conflict_keeps_the_edit() {
    let (_dir, repo) = fresh();
    let helper = entity(&repo, "helper_base");
    svc_repo::branch(&repo, "a").unwrap();
    repo.mutate(Op::Delete { id: helper, intent: Intent::Refactor }, None, |repo, cur| {
        repo.amend(cur, delete(cur, repo.store(), helper)?)
    })
    .unwrap();
    svc_repo::branch(&repo, "b").unwrap();
    edit(&repo, helper, "fn helper_base() { let _ = 1; }\n");
    let out = svc_repo::merge(&repo, "a").unwrap();
    let de: Vec<_> = out.conflicts.iter().filter(|c| matches!(c.conflict, Conflict::DeleteEdit { .. })).collect();
    assert_eq!(de.len(), 1, "{:?}", out.conflicts);
    assert!(repo.current().unwrap().entities.contains_key(&helper));
    svc_repo::resolve(&repo, de[0].n, Take::B).unwrap();
    assert!(!repo.current().unwrap().entities.contains_key(&helper), "take B = the merged-in branch's delete");
}

#[test]
fn both_edit_same_body_differently_is_a_content_conflict() {
    let (_dir, repo) = fresh();
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    edit(&repo, main, "fn main() {\n    let a = 1;\n    let _ = a;\n}\n");
    svc_repo::branch(&repo, "b").unwrap();
    edit(&repo, main, "fn main() {\n    let b = 2;\n    let _ = b;\n}\n");
    let out = svc_repo::merge(&repo, "a").unwrap();
    let content: Vec<_> = out.conflicts.iter().filter(|c| matches!(c.conflict, Conflict::Content { .. })).collect();
    assert_eq!(content.len(), 1, "{:?}", out.conflicts);
    svc_repo::resolve(&repo, content[0].n, Take::Base).unwrap();
    let cur = repo.current().unwrap();
    assert!(cur.conflicts.iter().all(|c| !matches!(c, Conflict::Content { .. })));
    let base = repo.store().get_snapshot(out.base).unwrap();
    assert_eq!(cur.entities[&main].content, base.entities[&main].content);
}

#[test]
fn both_edit_same_body_disjointly_merges_clean() {
    let (dir, repo) = fresh();
    let main = entity(&repo, "main");
    edit(&repo, main, "fn main() {\n    let _ = read(\"x\");\n    let one = 1;\n    let two = 2;\n    let three = 3;\n    let _ = (one, two, three);\n}\n");
    svc_repo::new(&repo).unwrap();
    svc_repo::branch(&repo, "a").unwrap();
    edit(&repo, main, "fn main() {\n    let _ = read(\"x\");\n    let one = 10;\n    let two = 2;\n    let three = 3;\n    let _ = (one, two, three);\n}\n");
    svc_repo::branch(&repo, "b").unwrap();
    edit(&repo, main, "fn main() {\n    let _ = read(\"x\");\n    let one = 1;\n    let two = 2;\n    let three = 30;\n    let _ = (one, two, three);\n}\n");
    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let t = text(&dir);
    assert!(t.contains("let one = 10;") && t.contains("let three = 30;"), "{t}");
}

#[test]
fn add_add_same_signature_unifies_and_rewrites_references() {
    let (dir, repo) = fresh();
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    let helper_a = add(&repo, "fn helper() -> u8 { 1 }\n");
    svc_repo::branch(&repo, "b").unwrap();
    let helper_b = add(&repo, "fn helper() -> u8 { 1 }\n");
    edit(&repo, main, "fn main() {\n    let _ = read(\"x\");\n    let _ = helper();\n}\n");

    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    assert_eq!(snap.entities.values().filter(|r| r.name == "helper").count(), 1);
    let survivor = snap.entities.keys().find(|id| **id == helper_a || **id == helper_b).copied().unwrap();
    let bytes = repo.store().get_bytes_blob(snap.entities[&main].bytes).unwrap();
    assert!(bytes.chunks().contains(&svc_core::Chunk::Name(survivor)), "main's call points at the surviving id");
    let t = text(&dir);
    assert_eq!(t.matches("fn helper()").count(), 1, "{t}");
    assert!(t.contains("let _ = helper();"), "{t}");
}

#[test]
fn merge_of_an_ancestor_is_refused_and_ordinals_stay_dense() {
    let (_dir, repo) = fresh();
    svc_repo::branch(&repo, "a").unwrap();
    svc_repo::branch(&repo, "b").unwrap();
    assert!(svc_repo::merge(&repo, "nope").is_err());
    add(&repo, "fn extra_b() {}\n");
    svc_repo::edit(&repo, "a").unwrap();
    add(&repo, "fn extra_a() {}\n");
    let out = svc_repo::merge(&repo, "b").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    let mut ords: Vec<u32> = snap.entities.values().map(|r| r.ordinal).collect();
    ords.sort();
    assert_eq!(ords, (0..snap.entities.len() as u32).collect::<Vec<_>>());
    assert!(svc_repo::merge(&repo, "b").is_err(), "already an ancestor");
}
