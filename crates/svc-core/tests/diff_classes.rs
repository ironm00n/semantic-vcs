//! `diff` reports the classifier's class for an edited entity. It used to label every
//! content change "binding-preserving" without looking, so `svc diff` contradicted
//! the op log for a shadowing edit.
use std::collections::BTreeMap;

use svc_core::engine::{diff, edit_def, lookup_name, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Delta, ObservedClass};

const SRC: &str = "struct Config { retries: u32 }\nfn canon(c: &Config) -> Config { Config { retries: c.retries } }\nfn validate(c: &Config) -> bool {\n    c.retries > 10\n}\n";

fn class_of(deltas: &[Delta]) -> Option<ObservedClass> {
    deltas.iter().find_map(|d| match d {
        Delta::Edited(_, c) => Some(*c),
        _ => None,
    })
}

#[test]
fn diff_reports_the_classifiers_class() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&base, "validate").unwrap();

    let (shadow, observed) = edit_def(
        &store,
        &langs,
        &base,
        id,
        b"fn validate(c: &Config) -> bool {\n    let c = &canon(c);\n    c.retries > 10\n}\n",
    )
    .unwrap();
    assert_eq!(observed, ObservedClass::BindingChanging);
    assert_eq!(
        class_of(&diff(&store, &base, &shadow).unwrap()),
        Some(ObservedClass::BindingChanging)
    );

    let (docs, _) = edit_def(
        &store,
        &langs,
        &base,
        id,
        b"fn validate(c: &Config) -> bool {\n    // at most ten\n    c.retries > 10\n}\n",
    )
    .unwrap();
    assert_eq!(
        class_of(&diff(&store, &base, &docs).unwrap()),
        Some(ObservedClass::DocsOnly)
    );

    let (alpha, _) = edit_def(
        &store,
        &langs,
        &base,
        id,
        b"fn validate(cfg: &Config) -> bool {\n    cfg.retries > 10\n}\n",
    )
    .unwrap();
    assert_eq!(
        class_of(&diff(&store, &base, &alpha).unwrap()),
        Some(ObservedClass::Alpha)
    );

    let (preserving, _) = edit_def(
        &store,
        &langs,
        &base,
        id,
        b"fn validate(c: &Config) -> bool {\n    c.retries > 5\n}\n",
    )
    .unwrap();
    assert_eq!(
        class_of(&diff(&store, &base, &preserving).unwrap()),
        Some(ObservedClass::BindingPreserving)
    );
}
