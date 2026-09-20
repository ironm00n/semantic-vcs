//! A changeset moves between two clones with its review state: after `transfer`, the
//! receiving store lists the group's ops with the same verdicts, subjects and times; a
//! second transfer sends nothing; a later op joins the same group and travels alone; the
//! other clone's own addition comes back the same way.

use std::path::Path;

use svc_core::engine::{classify_def, edit_def, rename};
use svc_core::{Intent, Op};
use svc_repo::sync::transfer;
use svc_repo::{Repo, changeset_begin, changeset_end, changeset_reopen, changesets, resolve_changeset};

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
    let op = Op::EditDef { id, definition: definition.into(), intent: Intent::Refactor };
    repo.mutate(op, Some(observed), |repo, cur| {
        let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
        repo.amend(cur, next)
    })
    .unwrap();
}

/// What both clones must agree on: everything about the group's ops except the op index
/// (each clone's own log position) and the entity id (each clone's own, mapped by path).
fn review_state(repo: &Repo, name: &str) -> Vec<String> {
    let cs = changesets(repo).unwrap().into_iter().find(|c| c.name == name).unwrap();
    cs.ops
        .iter()
        .map(|o| {
            let kind = match o.op {
                Op::Rename { .. } => "rename",
                Op::EditDef { .. } => "edit-def",
                _ => "other",
            };
            format!(
                "{kind} {:?} {:?} {} {:?} {}",
                o.declared,
                o.observed,
                o.flagged,
                o.subject,
                o.at
            )
        })
        .collect()
}

#[test]
fn a_changeset_travels_with_its_review_state_and_only_what_is_new_travels() {
    let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    demo_crate(a_dir.path());
    demo_crate(b_dir.path());
    let a = Repo::init(a_dir.path(), Repo::default_langs()).unwrap();
    let b = Repo::init(b_dir.path(), Repo::default_langs()).unwrap();

    let cs = changeset_begin(&a, "reviewed", Intent::Refactor, None, false).unwrap().id;
    rename_to(&a, "read", "read_file");
    edit(&a, "normalize", "fn normalize(s: &str) -> String {\n    s.trim().to_uppercase()\n}");
    changeset_end(&a).unwrap();
    assert_eq!(review_state(&a, "reviewed").len(), 2);
    assert!(changesets(&b).unwrap().is_empty());

    let r = transfer(&a, &b, cs).unwrap();
    assert_eq!(r.sent, 2);
    assert_eq!(r.import.unwrap().diverged_at, None);
    assert!(std::fs::read_to_string(b_dir.path().join("src/main.rs")).unwrap().contains("read_file("));
    assert_eq!(review_state(&a, "reviewed"), review_state(&b, "reviewed"));
    assert_eq!(resolve_changeset(&b, "reviewed").unwrap().id, cs);
    assert_eq!(transfer(&a, &b, cs).unwrap().sent, 0, "nothing new to send");

    // A follow-up joins the same group and is the only thing sent.
    changeset_reopen(&a, cs).unwrap();
    rename_to(&a, "validate", "check");
    changeset_end(&a).unwrap();
    assert_eq!(transfer(&a, &b, cs).unwrap().sent, 1);
    assert_eq!(review_state(&a, "reviewed"), review_state(&b, "reviewed"));

    // The reviewer's own op on b comes back to a.
    changeset_reopen(&b, cs).unwrap();
    rename_to(&b, "parse", "parse_config");
    changeset_end(&b).unwrap();
    assert_eq!(transfer(&b, &a, cs).unwrap().sent, 1);
    assert!(std::fs::read_to_string(a_dir.path().join("src/main.rs")).unwrap().contains("parse_config("));
    let state = review_state(&a, "reviewed");
    assert_eq!(state.len(), 4);
    assert_eq!(state, review_state(&b, "reviewed"));

    // A prefix or a name resolves; an unknown one is refused.
    assert_eq!(resolve_changeset(&a, &cs.short()[..4]).unwrap().id, cs);
    assert!(resolve_changeset(&a, "nope").is_err());
}
