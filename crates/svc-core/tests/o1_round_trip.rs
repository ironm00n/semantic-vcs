use svc_core::RustLang;
use svc_core::engine::{ingest_file, lookup_name, relocate, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::lang::Langs;
use svc_core::store::MemStore;

fn round_trip(src: &str) -> String {
    let store = MemStore::new();
    let lang = RustLang;
    let path = RelPath::new("src/lib.rs").unwrap();
    let snap = ingest_file(src.as_bytes(), path, &lang, &store, ChangeId::new()).unwrap();
    let langs = Langs::new(vec![Box::new(RustLang)]);
    let rendered = render(&snap, &store, &langs, false).unwrap();
    let bytes = rendered.files.values().next().cloned().unwrap_or_default();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn o1_simple_file() {
    let src = r#"struct Config { path: String }

fn read(path: &str) -> String {
    path.to_string()
}

fn main() {
    let _ = read("x");
}
"#;
    assert_eq!(round_trip(src), src);
}

#[test]
fn o1_impl_method() {
    let src = r#"struct Config;

impl Config {
    fn new() -> Self {
        Config
    }
}
"#;
    assert_eq!(round_trip(src), src);
}

#[test]
fn o1_use_and_trailing_newline() {
    let src = "use std::fmt;\n\nfn f() {}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
fn o1_opaque_manifest_round_trips_without_a_language() {
    use std::collections::BTreeMap;
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), b"fn f() {}\n".to_vec());
    files.insert(
        RelPath::new("Cargo.toml").unwrap(),
        b"[package]\nname = \"x\"\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let toml = RelPath::new("Cargo.toml").unwrap();
    assert_eq!(
        snap.files.get(&toml).unwrap().tail(&store).unwrap(),
        b"[package]\nname = \"x\"\n"
    );
    assert!(!snap.entities.values().any(|e| e.file == toml));
    let rendered = render(&snap, &store, &langs, false).unwrap();
    assert_eq!(
        rendered.files.get(&toml).unwrap().as_slice(),
        b"[package]\nname = \"x\"\n"
    );
}

#[test]
fn relocate_to_a_missing_path_is_still_rendered() {
    use std::collections::BTreeMap;
    let store = MemStore::new();
    let langs = rust_langs();
    let src = RelPath::new("src/lib.rs").unwrap();
    let dest = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(src.clone(), b"fn a() {}\nfn b() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "b").unwrap();
    let next = relocate(&snap, &store, id, dest.clone(), 0).unwrap();
    assert!(
        next.files.contains_key(&dest),
        "relocate must create a FileRecord"
    );
    let rendered = render(&next, &store, &langs, false).unwrap();
    let dest_src = String::from_utf8_lossy(rendered.files.get(&dest).unwrap());
    assert!(dest_src.contains("fn b"), "{dest_src}");
    let src_src = String::from_utf8_lossy(rendered.files.get(&src).unwrap());
    assert!(!src_src.contains("fn b"), "{src_src}");
}

#[test]
fn o1_svc_core_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    visit(&root, &mut files);
    assert!(!files.is_empty());
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let got = round_trip(&src);
        assert_eq!(got, src, "round-trip failed for {}", path.display());
    }
}

fn visit(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            visit(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}
