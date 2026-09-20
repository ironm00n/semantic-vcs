//! An edit on a binder's own line — a changed initializer, a `?` becoming
//! `.map_err(..)?` — leaves every later use of that binder bound to the same
//! declaration. The classifier used to call it binding-changing because the slot
//! bijection only paired binders whose declaration lines the diff kept equal, so
//! the binder was unmapped and each surviving use failed `same_target`.
use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{ObservedClass, Snapshot};

const SRC: &str = "fn resolve(x: &str) -> Result<u32, String> { x.parse().map_err(|e: std::num::ParseIntError| e.to_string()) }\n\
fn show(query: &str, at: Option<&str>) -> Result<u32, String> {\n\
    let snap = at.unwrap_or(query);\n\
    let id = resolve(snap)?;\n\
    let twice = id * 2;\n\
    Ok(twice + id)\n\
}\n";

fn base() -> (MemStore, Snapshot) {
    let store = MemStore::new();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &rust_langs(), &files, None, ChangeId::new()).unwrap();
    (store, snap)
}

fn class(def: &str) -> ObservedClass {
    let (store, snap) = base();
    let id = lookup_name(&snap, "show").unwrap();
    edit_def(&store, &rust_langs(), &snap, id, def.as_bytes()).unwrap().1
}

#[test]
fn editing_the_binder_line_keeps_its_uses_binding_preserving() {
    let class = class(
        "fn show(query: &str, at: Option<&str>) -> Result<u32, String> {\n\
    let snap = at.unwrap_or(query);\n\
    let id = resolve(snap).map_err(|e| format!(\"show: {e}\"))?;\n\
    let twice = id * 2;\n\
    Ok(twice + id)\n\
}\n",
    );
    assert_eq!(class, ObservedClass::BindingPreserving);
}

#[test]
fn a_changed_initializer_is_binding_preserving() {
    let class = class(
        "fn show(query: &str, at: Option<&str>) -> Result<u32, String> {\n\
    let snap = at.unwrap_or(\"head\");\n\
    let id = resolve(snap)?;\n\
    let twice = id * 2;\n\
    Ok(twice + id)\n\
}\n",
    );
    assert_eq!(class, ObservedClass::BindingPreserving);
}

#[test]
fn shadowing_between_binder_and_use_is_still_binding_changing() {
    let class = class(
        "fn show(query: &str, at: Option<&str>) -> Result<u32, String> {\n\
    let snap = at.unwrap_or(query);\n\
    let id = resolve(snap)?;\n\
    let id = id + 1;\n\
    let twice = id * 2;\n\
    Ok(twice + id)\n\
}\n",
    );
    assert_eq!(class, ObservedClass::BindingChanging);
}

#[test]
fn shadowing_while_editing_the_binder_line_is_still_binding_changing() {
    // Two `id` binders on the new side: the surviving `id * 2` cannot be paired by name,
    // so it is not assumed to be the same declaration.
    let class = class(
        "fn show(query: &str, at: Option<&str>) -> Result<u32, String> {\n\
    let snap = at.unwrap_or(query);\n\
    let id = resolve(snap).map_err(|e| format!(\"show: {e}\"))?;\n\
    let id = id + 1;\n\
    let twice = id * 2;\n\
    Ok(twice + id)\n\
}\n",
    );
    assert_eq!(class, ObservedClass::BindingChanging);
}
