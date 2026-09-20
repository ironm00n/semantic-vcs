//! A store's op log, exported as a bundle and replayed into a fresh store over the same
//! tree, reproduces every tree the log records — through renames, a typed edit, a hand
//! edit, a branch, a conflicting merge, its resolution and an undo — and the replayed
//! store answers svc's questions about real entities.

use std::path::Path;

use svc_core::engine::{classify_def, edit_def, rename};
use svc_core::{Op, OpIx, Take};
use svc_repo::bundle::{self, Bundle};
use svc_repo::{Repo, workspace};

const HERE: &str = env!("CARGO_MANIFEST_DIR");

fn demo_crate(into: &Path) {
    let src = Path::new(HERE).join("../../demo/config");
    for name in ["Cargo.toml", "config.txt"] {
        std::fs::copy(src.join(name), into.join(name)).unwrap();
    }
    std::fs::create_dir_all(into.join("src")).unwrap();
    std::fs::copy(src.join("src/main.rs"), into.join("src/main.rs")).unwrap();
}

fn rename_to(repo: &Repo, from: &str, to: &str) {
    let id = svc_repo::resolve_entity(repo, from).unwrap();
    repo.mutate(Op::Rename { id, new: to.into() }, None, |repo, cur| repo.amend(cur, rename(cur, id, to)?))
        .unwrap();
}

fn edit(repo: &Repo, entity: &str, definition: &str) {
    let id = svc_repo::resolve_entity(repo, entity).unwrap();
    let cur = repo.current().unwrap();
    let observed = classify_def(repo.store(), repo.langs(), &cur, id, definition.as_bytes()).unwrap();
    let op = Op::EditDef { id, definition: definition.into(), intent: svc_core::Intent::Refactor };
    repo.mutate(op, Some(observed), |repo, cur| {
        let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
        repo.amend(cur, next)
    })
    .unwrap();
}

#[test]
fn a_bundle_replays_the_whole_story_into_a_fresh_store_over_the_same_tree() {
    let a = tempfile::tempdir().unwrap();
    demo_crate(a.path());
    let repo = Repo::init(a.path(), Repo::default_langs()).unwrap();
    svc_repo::new(&repo).unwrap();
    rename_to(&repo, "parse", "parse_config");
    edit(&repo, "normalize", "fn normalize(s: &str) -> String {\n    s.trim().to_lowercase()\n}");
    // A hand edit to an opaque file, absorbed.
    std::fs::write(a.path().join("config.txt"), "hand-edited\n").unwrap();
    svc_repo::status(&repo).unwrap();
    // A second line of work on the same entity, merged back with a conflict.
    let trunk = repo.current_change().unwrap();
    svc_repo::branch(&repo, "side").unwrap();
    edit(&repo, "normalize", "fn normalize(s: &str) -> String {\n    s.trim().to_uppercase()\n}");
    let side = repo.current_change().unwrap();
    svc_repo::checkout(&repo, repo.store().head(trunk).unwrap()).unwrap();
    let m = svc_repo::merge(&repo, &side.to_string()).unwrap();
    assert_eq!(m.conflicts.len(), 1, "{:?}", m.conflicts);
    svc_repo::resolve(&repo, 0, Take::B).unwrap();
    rename_to(&repo, "load", "load_config");
    svc_repo::undo(&repo).unwrap();
    let recorded = svc_repo::op_log(&repo).unwrap().len();

    let bundle = bundle::export(&repo, OpIx(1)).unwrap();
    assert_eq!(bundle.entries.len(), recorded - 1, "everything after init");
    let text = serde_json::to_string(&bundle).unwrap();
    assert!(text.contains("hand-edited"), "the absorbed file travels with its op");
    let bundle: Bundle = serde_json::from_str(&text).unwrap();

    // A fresh store over the same bytes: every entity has a new id; the paths match.
    let b = tempfile::tempdir().unwrap();
    demo_crate(b.path());
    let fresh = Repo::init(b.path(), Repo::default_langs()).unwrap();
    let report = bundle::import(&fresh, &bundle).unwrap();
    assert_eq!(report.applied, bundle.entries.len());
    assert_eq!(report.diverged_at, None, "every tree after every op is the recorded one");

    let text = std::fs::read_to_string(b.path().join("src/main.rs")).unwrap();
    assert!(text.contains("fn parse_config(") && text.contains("to_uppercase") && text.contains("fn load("), "{text}");
    assert_eq!(std::fs::read_to_string(b.path().join("config.txt")).unwrap(), "hand-edited\n");
    let log = svc_repo::op_log(&fresh).unwrap();
    assert_eq!(log.len(), recorded, "same number of ops, init included");
    let kinds: Vec<String> = log.iter().rev().map(|o| format!("{:?}", o.op).split(' ').next().unwrap().trim_end_matches('{').to_string()).collect();
    let recorded_kinds: Vec<String> = svc_repo::op_log(&repo).unwrap().iter().rev().map(|o| format!("{:?}", o.op).split(' ').next().unwrap().trim_end_matches('{').to_string()).collect();
    assert_eq!(kinds, recorded_kinds);
    let id = svc_repo::resolve_entity(&fresh, "normalize").unwrap();
    let blame = svc_repo::blame(&fresh, id).unwrap();
    assert!(blame.len() >= 3, "added, edited, merged/resolved: {blame:?}");
    assert!(svc_repo::replay(&fresh).unwrap().ok());
    assert!(!workspace::list(&fresh).unwrap()[0].stale);
}

#[test]
fn import_refuses_a_tree_that_is_not_the_bundles_base() {
    let a = tempfile::tempdir().unwrap();
    demo_crate(a.path());
    let repo = Repo::init(a.path(), Repo::default_langs()).unwrap();
    rename_to(&repo, "parse", "parse_config");
    let bundle = bundle::export(&repo, OpIx(1)).unwrap();

    let b = tempfile::tempdir().unwrap();
    demo_crate(b.path());
    std::fs::write(b.path().join("config.txt"), "different base\n").unwrap();
    let other = Repo::init(b.path(), Repo::default_langs()).unwrap();
    let err = bundle::import(&other, &bundle).err().expect("refused");
    assert!(err.to_string().contains("not the bundle's base"), "{err}");
}
