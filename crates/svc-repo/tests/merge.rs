use std::collections::BTreeMap;

use svc_core::{
    ByteRange, Bytes, Chunk, Conflict, Content, EntityId, EntityRecord, Intent, Kind, Op, RelPath,
    Token,
};
use svc_repo::{Repo, Take};

const LIB: &str = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n\nfn main() {\n    let _ = read(\"x\");\n}\n";

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

fn rename(repo: &Repo, id: EntityId, new: &str) {
    repo.mutate(Op::Rename { id, new: new.into() }, None, |repo, cur| {
        let mut next = cur.clone();
        next.entities.get_mut(&id).unwrap().name = new.into();
        repo.amend(cur, next)
    })
    .unwrap();
}

/// Replace an entity's body with literal text (no holes) — stands in for `edit-def`.
fn set_body(repo: &Repo, id: EntityId, text: &str, refs: &[EntityId]) {
    let mut chunks = vec![Chunk::Literal(ByteRange::new(0, text.len() as u32))];
    let mut tokens = vec![Token::Lit(text.into())];
    for r in refs {
        chunks.push(Chunk::Name(*r));
        tokens.push(Token::Ident(svc_core::IdentRef::Entity(*r)));
    }
    let bytes = Bytes::new(text.as_bytes().to_vec(), chunks, Vec::new()).unwrap();
    let content = Content { tokens };
    let bytes_id = repo.store().put_bytes_blob(&bytes).unwrap();
    let content_id = repo.store().put_content(&content).unwrap();
    repo.mutate(
        Op::EditDef { id, definition: text.into(), intent: Intent::Feature },
        None,
        |repo, cur| {
            let mut next = cur.clone();
            let rec = next.entities.get_mut(&id).unwrap();
            rec.bytes = bytes_id;
            rec.content = content_id;
            repo.amend(cur, next)
        },
    )
    .unwrap();
}

fn add_fn(repo: &Repo, id: EntityId, name: &str, text: &str) {
    let bytes = Bytes::new(
        text.as_bytes().to_vec(),
        vec![Chunk::Literal(ByteRange::new(0, text.len() as u32))],
        Vec::new(),
    )
    .unwrap();
    let content = Content { tokens: vec![Token::Lit(text.into())] };
    let bytes_id = repo.store().put_bytes_blob(&bytes).unwrap();
    let content_id = repo.store().put_content(&content).unwrap();
    repo.mutate(
        Op::AddDef { id, parent: None, ordinal: 99, definition: text.into(), intent: Intent::Feature },
        None,
        |repo, cur| {
            let mut next = cur.clone();
            next.entities.insert(
                id,
                EntityRecord {
                    name: name.into(),
                    kind: Kind::Fn,
                    parent: None,
                    file: RelPath::new("src/lib.rs").unwrap(),
                    ordinal: 99,
                    content: content_id,
                    bytes: bytes_id,
                },
            );
            repo.amend(cur, next)
        },
    )
    .unwrap();
}

#[test]
fn demo_line_5_rename_on_a_edit_on_b_merges_clean() {
    let (dir, repo) = fresh();
    let read = entity(&repo, "read");
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    rename(&repo, read, "read_file");
    svc_repo::branch(&repo, "b").unwrap();
    assert_eq!(repo.current().unwrap().entities[&read].name, "read", "b is a sibling of a");
    set_body(&repo, main, "fn main() { let _ = ", &[read]);

    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    assert_eq!(snap.change, out.change);
    assert_eq!(snap.parents.len(), 2);
    assert_eq!(snap.entities[&read].name, "read_file");
    let text = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(text.contains("fn read_file(path"), "{text}");
    assert!(text.contains("fn main() { let _ = read_file"), "B's call renders A's name: {text}");
    assert!(repo.working_copy_clean().unwrap());
    let log = svc_repo::log(&repo, None).unwrap();
    assert!(matches!(log[0].op, Op::Merge { .. }));
}

#[test]
fn rename_rename_is_an_attr_conflict_resolved_by_take() {
    let (_dir, repo) = fresh();
    let read = entity(&repo, "read");
    svc_repo::branch(&repo, "a").unwrap();
    rename(&repo, read, "load");
    svc_repo::branch(&repo, "b").unwrap();
    rename(&repo, read, "fetch");
    let out = svc_repo::merge(&repo, "a").unwrap();
    assert_eq!(out.conflicts.len(), 1);
    assert!(matches!(out.conflicts[0].conflict, Conflict::Attr { .. }));
    assert_eq!(repo.current().unwrap().entities[&read].name, "fetch", "A = current side wins provisionally");
    assert_eq!(svc_repo::conflicts(&repo).unwrap().len(), 1);

    let resolved = svc_repo::resolve(&repo, 0, Take::B).unwrap();
    assert!(resolved.conflicts.is_empty());
    assert_eq!(repo.current().unwrap().entities[&read].name, "load");
    assert_eq!(repo.current().unwrap().change, out.change, "resolve amends the merge change");
    assert!(svc_repo::resolve(&repo, 0, Take::A).is_err());
}

#[test]
fn delete_edit_conflict_keeps_the_edit() {
    let (_dir, repo) = fresh();
    let read = entity(&repo, "read");
    svc_repo::branch(&repo, "a").unwrap();
    repo.mutate(Op::Delete { id: read, intent: Intent::Refactor }, None, |repo, cur| {
        let mut next = cur.clone();
        next.entities.remove(&read);
        repo.amend(cur, next)
    })
    .unwrap();
    svc_repo::branch(&repo, "b").unwrap();
    set_body(&repo, read, "fn read() {}\n", &[]);
    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(matches!(out.conflicts[0].conflict, Conflict::DeleteEdit { .. }));
    assert!(repo.current().unwrap().entities.contains_key(&read));
    svc_repo::resolve(&repo, 0, Take::B).unwrap();
    assert!(!repo.current().unwrap().entities.contains_key(&read), "take B = A-side's delete (B here is the merged-in branch a)");
}

#[test]
fn both_edit_same_body_is_a_content_conflict_until_body_merge_lands() {
    let (_dir, repo) = fresh();
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    set_body(&repo, main, "fn main() { a() }\n", &[]);
    svc_repo::branch(&repo, "b").unwrap();
    set_body(&repo, main, "fn main() { b() }\n", &[]);
    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(matches!(out.conflicts[0].conflict, Conflict::Content { .. }));
    svc_repo::resolve(&repo, 0, Take::Base).unwrap();
    let cur = repo.current().unwrap();
    assert!(cur.conflicts.is_empty());
    let base = repo.store().get_snapshot(out.base).unwrap();
    assert_eq!(cur.entities[&main].bytes, base.entities[&main].bytes);
}

#[test]
fn add_add_same_signature_unifies_and_rewrites_references() {
    let (_dir, repo) = fresh();
    let main = entity(&repo, "main");
    svc_repo::branch(&repo, "a").unwrap();
    let helper_a = EntityId::new();
    add_fn(&repo, helper_a, "helper", "fn helper() {}\n");
    svc_repo::branch(&repo, "b").unwrap();
    let helper_b = EntityId::new();
    add_fn(&repo, helper_b, "helper", "fn helper() {}\n");
    set_body(&repo, main, "fn main() { ", &[helper_b]);

    let out = svc_repo::merge(&repo, "a").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    // current = side A = branch b; merged-in = side B = branch a. B's id folds into A's.
    assert_eq!(out.unified, vec![(helper_a, helper_b)]);
    assert!(snap.entities.contains_key(&helper_b));
    assert!(!snap.entities.contains_key(&helper_a));
    let bytes = repo.store().get_bytes_blob(snap.entities[&main].bytes).unwrap();
    assert!(bytes.chunks().contains(&Chunk::Name(helper_b)));
    assert_eq!(snap.entities.values().filter(|r| r.name == "helper").count(), 1);
}

#[test]
fn merge_of_an_ancestor_is_refused_and_ordinals_stay_dense() {
    let (_dir, repo) = fresh();
    svc_repo::branch(&repo, "a").unwrap();
    let a_change = repo.current_change().unwrap();
    svc_repo::branch(&repo, "b").unwrap();
    assert!(svc_repo::merge(&repo, "nope").is_err());
    let _ = a_change;
    let extra = EntityId::new();
    add_fn(&repo, extra, "extra", "fn extra() {}\n");
    svc_repo::edit(&repo, "a").unwrap();
    let extra_a = EntityId::new();
    add_fn(&repo, extra_a, "extra_a", "fn extra_a() {}\n");
    let out = svc_repo::merge(&repo, "b").unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let snap = repo.current().unwrap();
    let mut ords: Vec<u32> = snap.entities.values().map(|r| r.ordinal).collect();
    ords.sort();
    assert_eq!(ords, (0..snap.entities.len() as u32).collect::<Vec<_>>());
    assert!(svc_repo::merge(&repo, "b").is_err(), "already an ancestor");
    let _ = BTreeMap::<u8, u8>::new();
}
