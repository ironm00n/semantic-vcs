use std::collections::BTreeMap;

use svc_core::ObservedClass;
use svc_core::engine::{edit_def, lookup_name, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;

#[test]
fn o8_shadowing_let_is_binding_changing() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = r#"struct Config { retries: u32 }
fn canon(c: &Config) -> Config { Config { retries: c.retries } }
fn validate(c: &Config) -> bool {
    c.retries > 10
}
"#;
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "validate").unwrap();
    let new = b"fn validate(c: &Config) -> bool {
    let c = &canon(c);
    c.retries > 10
}
";
    let (_, class) = edit_def(&store, &langs, &snap, id, new).unwrap();
    assert_eq!(class, ObservedClass::BindingChanging);
}

#[test]
fn o8_demo_validate_shadow_is_binding_changing() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = r#"struct Config { path: String, retries: u32 }
struct Error(String);
fn canon(c: &Config) -> Config { Config { path: c.path.trim().to_owned(), retries: c.retries } }
fn validate(c: &Config) -> Result<(), Error> {
    if c.retries > 10 {
        return Err(Error("retries must not exceed 10".into()));
    }
    Ok(())
}
"#;
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "validate").unwrap();
    let new = b"fn validate(c: &Config) -> Result<(), Error> {
    let c = &canon(c);
    if c.retries > 10 {
        return Err(Error(\"retries must not exceed 10\".into()));
    }
    Ok(())
}
";
    let (_, class) = edit_def(&store, &langs, &snap, id, new).unwrap();
    assert_eq!(class, ObservedClass::BindingChanging);
}

#[test]
fn o8_unused_local_is_binding_preserving() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "fn validate(c: &u32) -> bool { *c > 10 }\n";
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "validate").unwrap();
    let new = b"fn validate(c: &u32) -> bool { let _t = 1; *c > 10 }\n";
    let (_, class) = edit_def(&store, &langs, &snap, id, new).unwrap();
    assert_eq!(class, ObservedClass::BindingPreserving);
}

#[test]
fn o8_demo_validate_without_trailing_newline() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/main.rs").unwrap();
    let src = include_bytes!("../../../demo/config/src/main.rs");
    let mut files = BTreeMap::new();
    files.insert(path, src.to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "validate").unwrap();
    let new = b"fn validate(c: &Config) -> Result<(), Error> {
    let c = &canon(c);
    if c.retries > 10 {
        return Err(Error(\"retries must not exceed 10\".into()));
    }
    Ok(())
}";
    assert_ne!(new.last(), Some(&b'\n'));
    let (_, class) = edit_def(&store, &langs, &snap, id, new).unwrap();
    assert_eq!(class, ObservedClass::BindingChanging);
}

#[test]
fn o8_serde_attribute_is_not_docs_only() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/ids.rs").unwrap();
    let src = "struct RelPath(String);\n";
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "RelPath").unwrap();
    let (_, class) = edit_def(
        &store,
        &langs,
        &snap,
        id,
        b"#[serde(try_from = \"String\")]\nstruct RelPath(String);\n",
    )
    .unwrap();
    assert_eq!(
        class,
        ObservedClass::BindingPreserving,
        "a serde attribute changes compilation, not docs"
    );
}

#[test]
fn o8_doc_attribute_is_still_docs_only() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/ids.rs").unwrap();
    let src = "struct RelPath(String);\n";
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "RelPath").unwrap();
    let (_, class) = edit_def(
        &store,
        &langs,
        &snap,
        id,
        b"#[doc = \"a relative path\"]\nstruct RelPath(String);\n",
    )
    .unwrap();
    assert_eq!(class, ObservedClass::DocsOnly);
}
