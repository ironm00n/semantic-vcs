//! O6: rendering and re-ingesting a snapshot reaches a fixpoint without
//! churning entity identity.

use svc_core::RustLang;
use svc_core::engine::{ingest_file, ingest_file_prev, render};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::lang::{Env, Langs};
use svc_core::store::MemStore;

#[test]
fn o6_render_parse_render_is_a_fixpoint() {
    let source = br#"struct Config { path: String }

impl Config {
    fn load(path: &str) -> Self {
        let path = path.to_string();
        Self { path }
    }
}

fn main() {
    let _ = Config::load("svc.toml");
}
"#;
    let store = MemStore::new();
    let path = RelPath::new("src/lib.rs").unwrap();
    let first = ingest_file(source, path.clone(), &RustLang, &store, ChangeId::new()).unwrap();
    let langs = Langs::new(vec![Box::new(RustLang)]);
    let first_render = render(&first, &store, &langs, false).unwrap();
    let bytes = first_render.files.get(&path).unwrap();

    let second = ingest_file_prev(
        bytes,
        path.clone(),
        &RustLang,
        &store,
        ChangeId::new(),
        Some(&first),
        &Env::default(),
    )
    .unwrap();
    let second_render = render(&second, &store, &langs, false).unwrap();

    assert_eq!(second_render.files, first_render.files);
    assert_eq!(
        second.entities.keys().collect::<Vec<_>>(),
        first.entities.keys().collect::<Vec<_>>(),
        "a render/parse cycle must preserve entity identity"
    );
}
