//! M3: `impl` with attributes keeps its id; associated consts/types under
//! impl/trait stay out of the file/crate name maps (same family as nested
//! fns and `#[cfg(test)] mod tests`).
use std::collections::BTreeMap;

use svc_core::content::{IdentRef, Namespace};
use svc_core::engine::{
    env_from_snapshot, lookup_name, rename, render, rust_langs, snapshot_files, status_report,
};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Delta, Kind, Store};

fn snap(
    store: &MemStore,
    files: &BTreeMap<RelPath, Vec<u8>>,
    prev: Option<&svc_core::Snapshot>,
) -> svc_core::Snapshot {
    snapshot_files(store, &rust_langs(), files, prev, ChangeId::new()).unwrap()
}

fn impl_named<'a>(
    snap: &'a svc_core::Snapshot,
    name: &str,
) -> (svc_core::ids::EntityId, &'a svc_core::EntityRecord) {
    snap.entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl && r.name == name)
        .map(|(id, r)| (*id, r))
        .unwrap_or_else(|| panic!("missing {name}: {:?}", names(snap)))
}

fn names(snap: &svc_core::Snapshot) -> Vec<(String, Kind, Option<String>)> {
    snap.entities
        .iter()
        .map(|(_, r)| {
            let parent = r
                .parent
                .and_then(|p| snap.entities.get(&p).map(|x| x.name.clone()));
            (r.name.clone(), r.kind, parent)
        })
        .collect()
}

fn named_in_file(snap: &svc_core::Snapshot, name: &str, file: &RelPath) -> svc_core::ids::EntityId {
    snap.entities
        .iter()
        .find(|(_, rec)| rec.name == name && rec.file == *file && rec.parent.is_none())
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("{name} in {file}: {:?}", names(snap)))
}

#[test]
fn impl_keeps_id_when_an_attribute_is_added_above_it() {
    let store = MemStore::new();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nimpl S {\n    fn f() {}\n}\n".to_vec(),
    );
    let before = snap(&store, &files, None);
    let (impl_id, _) = impl_named(&before, "impl<S>");
    let method = before
        .entities
        .iter()
        .find(|(_, r)| r.name == "f" && r.parent == Some(impl_id))
        .map(|(id, _)| *id)
        .expect("method");

    files.insert(
        path,
        b"struct S;\n#[allow(dead_code)]\nimpl S {\n    fn f() {}\n}\n".to_vec(),
    );
    let after = snap(&store, &files, Some(&before));
    assert!(
        after.entities.contains_key(&impl_id),
        "impl reminted: before {:?} after {:?}",
        names(&before),
        names(&after)
    );
    assert!(
        after.entities.contains_key(&method),
        "method reminted: {:?}",
        names(&after)
    );
    let rep = status_report(&store, &before, &after).unwrap();
    assert!(
        !rep.deltas
            .iter()
            .any(|d| matches!(d, Delta::Added(_) | Delta::Removed { .. })),
        "attribute must not remint: {:?}",
        rep.deltas
    );
}

#[test]
fn attributed_impl_const_is_not_a_crate_name_collision() {
    // M3: `#[cfg(test)] impl S { const parse: u8 = 1; }` must not make a unique
    // crate-level `parse` look ambiguous to a third file.
    let store = MemStore::new();
    let langs = rust_langs();
    let impl_file = RelPath::new("crates/svc/src/a.rs").unwrap();
    let def = RelPath::new("crates/svc/src/b.rs").unwrap();
    let use_file = RelPath::new("crates/svc/src/c.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        impl_file.clone(),
        b"struct S;\n#[cfg(test)]\nimpl S {\n    const parse: u8 = 1;\n}\n".to_vec(),
    );
    files.insert(def.clone(), b"fn parse() {}\n".to_vec());
    files.insert(use_file.clone(), b"fn go() { parse(); }\n".to_vec());
    let s = snap(&store, &files, None);
    let id = named_in_file(&s, "parse", &def);
    let next = rename(&store, &s, id, "parse_b").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let c_txt = String::from_utf8(rendered.files[&use_file].clone()).unwrap();
    let a_txt = String::from_utf8(rendered.files[&impl_file].clone()).unwrap();
    assert!(c_txt.contains("parse_b();"), "{c_txt}");
    assert!(
        a_txt.contains("const parse: u8"),
        "associated const stays: {a_txt}"
    );
    assert!(!a_txt.contains("parse_b"), "{a_txt}");
}

#[test]
fn associated_const_in_impl_is_not_in_the_file_env() {
    let store = MemStore::new();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nimpl S {\n    const N: u8 = 1;\n}\nfn h() { let _ = N; }\n".to_vec(),
    );
    let s = snap(&store, &files, None);
    let mut env = env_from_snapshot(&s);
    env.current_file = Some(path);
    assert!(
        env.lookup("N", Namespace::Value).is_none(),
        "associated const must not occupy the file map: {:?}",
        env.lookup("N", Namespace::Value)
    );
    let h = lookup_name(&s, "h").unwrap();
    let content = store.get_content(s.entities[&h].content).unwrap();
    let n_ids: Vec<_> = s
        .entities
        .iter()
        .filter(|(_, r)| r.name == "N")
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(n_ids.len(), 1, "{:?}", names(&s));
    assert!(
        !content.tokens.iter().any(
            |t| matches!(t, svc_core::Token::Ident(IdentRef::Entity(id)) if n_ids.contains(&id))
        ),
        "bare N in h must not be the associated const: {:?}",
        content.tokens
    );
}

#[test]
fn associated_type_in_impl_is_not_in_the_file_env() {
    let store = MemStore::new();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nimpl S {\n    type Item = u8;\n}\nfn h() -> Item { 1 }\n".to_vec(),
    );
    let s = snap(&store, &files, None);
    let mut env = env_from_snapshot(&s);
    env.current_file = Some(path);
    assert!(
        env.lookup("Item", Namespace::Type).is_none(),
        "associated type must not occupy the file map: {:?}",
        env.lookup("Item", Namespace::Type)
    );
}

#[test]
fn associated_type_in_a_method_sig_is_the_impl_item() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nimpl S {\n    type Item = u8;\n    fn f() -> Item { 1 }\n}\nfn h() -> Item { 1 }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let item = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "Item" && r.parent.is_some())
        .map(|(id, _)| *id)
        .expect("associated type");
    let next = rename(&store, &s, item, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem = u8"), "{text}");
    assert!(text.contains("fn f() -> Elem"), "{text}");
    assert!(
        text.contains("fn h() -> Item"),
        "file-level Item must stay: {text}"
    );
}

#[test]
fn rename_of_associated_type_rewrites_self_and_type_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nstruct Other;\nimpl S {\n    type Item = u8;\n    fn f() -> Self::Item { 1 }\n    fn g() -> S::Item { 1 }\n}\nimpl Other {\n    type Item = u16;\n    fn h() -> Other::Item { 1 }\n}\nfn free() -> Item { 1 }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let (impl_s, _) = impl_named(&s, "impl<S>");
    let item = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "Item" && r.parent == Some(impl_s))
        .map(|(id, _)| *id)
        .expect("associated type");
    let next = rename(&store, &s, item, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem = u8"), "{text}");
    assert!(text.contains("Self::Elem"), "{text}");
    assert!(text.contains("S::Elem"), "{text}");
    assert!(
        text.contains("type Item = u16") && text.contains("Other::Item"),
        "the other impl's Item must stay: {text}"
    );
    assert!(
        text.contains("fn free() -> Item"),
        "file-level Item must stay: {text}"
    );
    assert!(!text.contains("Self::Item"), "{text}");
    assert!(!text.contains("S::Item"), "{text}");
}

#[test]
fn rename_of_associated_const_rewrites_self_and_type_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"struct S;\nstruct Other;\nimpl S {\n    const N: u8 = 1;\n    fn f() -> u8 { Self::N }\n    fn g() -> u8 { S::N }\n}\nimpl Other {\n    const N: u8 = 2;\n    fn h() -> u8 { Other::N }\n}\nfn free() { let _ = N; }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let (impl_s, _) = impl_named(&s, "impl<S>");
    let n = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "N" && r.parent == Some(impl_s))
        .map(|(id, _)| *id)
        .expect("associated const");
    let next = rename(&store, &s, n, "M").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("const M: u8 = 1"), "{text}");
    assert!(text.contains("Self::M"), "{text}");
    assert!(text.contains("S::M"), "{text}");
    assert!(
        text.contains("const N: u8 = 2") && text.contains("Other::N"),
        "the other impl's N must stay: {text}"
    );
    assert!(
        text.contains("let _ = N;"),
        "bare N outside the impl must stay: {text}"
    );
    assert!(!text.contains("Self::N"), "{text}");
    assert!(!text.contains("S::N"), "{text}");
}

#[test]
fn rename_of_trait_associated_type_rewrites_self_and_trait_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"trait T {\n    type Item;\n    fn f() -> Self::Item;\n    fn g() -> T::Item;\n    fn k() -> Item;\n}\ntrait Other {\n    type Item;\n    fn h() -> Other::Item;\n}\nfn free() -> Item { loop {} }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let tr = s
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Trait && r.name == "T")
        .map(|(id, _)| *id)
        .expect("trait T");
    let item = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "Item" && r.parent == Some(tr))
        .map(|(id, _)| *id)
        .expect("associated type");
    let next = rename(&store, &s, item, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem;"), "{text}");
    assert!(text.contains("Self::Elem"), "{text}");
    assert!(text.contains("T::Elem"), "{text}");
    assert!(text.contains("fn k() -> Elem"), "{text}");
    assert!(
        text.contains("type Item;") && text.contains("Other::Item"),
        "the other trait's Item must stay: {text}"
    );
    assert!(
        text.contains("fn free() -> Item"),
        "file-level Item must stay: {text}"
    );
    assert!(!text.contains("Self::Item"), "{text}");
    assert!(!text.contains("T::Item"), "{text}");
}

#[test]
fn rename_of_impl_trait_associated_type_rewrites_trait_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"trait Tr {\n    type Item;\n}\nstruct S;\nimpl Tr for S {\n    type Item = u8;\n    fn f() -> Self::Item { 1 }\n    fn g() -> S::Item { 1 }\n    fn h() -> Tr::Item { 1 }\n}\nfn free() -> Item { 1 }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let (impl_s, _) = impl_named(&s, "impl<Tr for S>");
    let item = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "Item" && r.parent == Some(impl_s))
        .map(|(id, _)| *id)
        .expect("impl associated type");
    let next = rename(&store, &s, item, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem = u8"), "{text}");
    assert!(text.contains("Self::Elem"), "{text}");
    assert!(text.contains("S::Elem"), "{text}");
    assert!(text.contains("Tr::Elem"), "{text}");
    assert!(
        text.contains("type Item;") && text.contains("fn free() -> Item"),
        "trait decl and file-level Item must stay: {text}"
    );
    assert!(!text.contains("Self::Item"), "{text}");
    assert!(!text.contains("S::Item"), "{text}");
    assert!(!text.contains("Tr::Item"), "{text}");
}

#[test]
fn rename_of_impl_trait_associated_type_rewrites_ufcs() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"trait Tr {\n    type Item;\n}\nstruct S;\nimpl Tr for S {\n    type Item = u8;\n    fn f() -> <S as Tr>::Item { 1 }\n}\nfn outside() -> <S as Tr>::Item { 1 }\nfn free() -> Item { 1 }\n"
            .to_vec(),
    );
    let s = snap(&store, &files, None);
    let (impl_s, _) = impl_named(&s, "impl<Tr for S>");
    let item = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "Item" && r.parent == Some(impl_s))
        .map(|(id, _)| *id)
        .expect("impl associated type");
    let next = rename(&store, &s, item, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem = u8"), "{text}");
    assert!(
        text.contains("fn f() -> <S as Tr>::Elem"),
        "UFCS inside the impl follows: {text}"
    );
    assert!(
        text.contains("fn outside() -> <S as Tr>::Item"),
        "UFCS outside the impl stays type-relative: {text}"
    );
    assert!(
        text.contains("type Item;") && text.contains("fn free() -> Item"),
        "trait decl and file-level Item must stay: {text}"
    );
}
