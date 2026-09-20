//! O9 for JavaScript — node as the binding oracle.
//!
//! "α-rename every JS local in the repo's own `.mjs` tree to a
//! guaranteed-fresh name, render the result, and run `node --check` plus a
//! load/run smoke test. If our binder table misses an occurrence, binds
//! something that is actually a reference, or renames two distinct bindings
//! to one name, the file stops parsing or the smoke test stops behaving."
//!
//! Scope: `Namespace::Value` locals (params, `let`/`const` bindings,
//! arrow params, catch params, import bindings) across the repo's whole
//! `.mjs` tree (`harness/svc-tools.mjs`,
//! `crates/svc-agent/tests/fake_agent.mjs` — that is all of it today; the
//! walk below picks up any new `.mjs` under those roots). Property keys,
//! method names, and `this` are never renameable spellings: `this` is
//! skipped by literal text, and shorthand properties are expanded
//! (`{a}` -> `{a: _svc_N}`) so the key stays put while the value renames.
//!
//! Built only against the frozen top-level `svc_core::engine::*` functions
//! (`parse`, `extract`, `resolve`) plus `Resolution`'s public fields, so it
//! should not need edits as engine internals move underneath it.
//!
//! `#[ignore]`d in the unit suite: it shells out to `node`, which is too
//! slow for every `cargo test --workspace`. The acceptance gate
//! `demo/run.sh` runs it next to O9 and counts a failure; by hand:
//!   cargo test -p svc-core --test o9_js_compiler_oracle -- --ignored --nocapture
//!
//! Deliberately engine-API-level like O9 (not `svc init` + rename ops):
//! the oracle is after binder-table bugs in `lang_js.rs`, and going
//! through the store would blame the wrong lane on red.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use svc_core::content::{IdentRef, Namespace};
use svc_core::engine::{parse, resolve};
use svc_core::ids::{ByteRange, Slot};
use svc_core::lang::Env;
use svc_core::JsLang;

fn find_node<'t>(root: tree_sitter::Node<'t>, range: ByteRange) -> Option<tree_sitter::Node<'t>> {
    if root.start_byte() as u32 == range.start && root.end_byte() as u32 == range.end {
        return Some(root);
    }
    if (root.start_byte() as u32) > range.start || (root.end_byte() as u32) < range.end {
        return None;
    }
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(found) = find_node(child, range) {
            return Some(found);
        }
    }
    None
}

/// α-rename every `Namespace::Value` local in `src` to a fresh `_svc_N` name.
/// `next_id` is a shared, crate-wide counter so every rewritten name is
/// globally unique — "guaranteed-fresh".
fn alpha_rename_file(src: &str, lang: &JsLang, next_id: &mut u32) -> String {
    let bytes = src.as_bytes();
    let tree = parse(bytes, lang).unwrap();
    let env = Env::default();

    // One `resolve()` over the whole file, not per entity: it walks the
    // entire subtree, so a single call yields every slot and ref in one
    // consistent slot namespace. (Per-entity resolve + skipping containers,
    // the O9 shape, mismatches here: a declaration renamed in its own leaf
    // entity while its uses sit in a skipped container entity stay stale.
    // First live proof: `const string = …` renamed at its declaration while
    // the `string(…)` calls inside the `toolSpecs` array did not follow.)
    let res = resolve(tree.root_node(), bytes, lang, &env).unwrap();

    // (byte range in the ORIGINAL source) -> (fresh name, origin).
    let mut renames: Vec<(ByteRange, String, &'static str)> = Vec::new();

    // Slot -> fresh name for this file's single resolve() call.
    let mut slot_names: BTreeMap<Slot, String> = BTreeMap::new();
    for (range, slot, ns) in &res.slots {
        if *ns != Namespace::Value {
            continue;
        }
        let orig = std::str::from_utf8(&bytes[range.start as usize..range.end as usize]).unwrap();
        // `this` is a keyword receiver, not a renameable binder.
        if orig == "this" {
            continue;
        }
        // `arguments` is an implicit binding: renaming the use without a
        // declaration to hang it on would silently break the function.
        if orig == "arguments" {
            continue;
        }
        let name = format!("_svc_{next_id}");
        *next_id += 1;
        slot_names.insert(*slot, name.clone());
        renames.push((*range, name, "slot"));
    }
    for (range, ident) in &res.refs {
        if let IdentRef::Local(slot, Namespace::Value) = ident {
            if let Some(name) = slot_names.get(slot) {
                // Object shorthand (`{a}` / `const {a} = …`) is
                // simultaneously the (fixed) key and a value reference
                // to the local. A straight text swap would rename the
                // key too; expand it to `key: newname`. `find_node`
                // returns the OUTERMOST node whose span matches `range`,
                // and the shorthand wrapper's span equals its wrapped
                // identifier's span exactly, so the match lands on the
                // wrapper itself, not a child.
                let is_shorthand = find_node(tree.root_node(), *range)
                    .is_some_and(|n| n.kind().contains("shorthand"));
                if is_shorthand {
                    let orig =
                        std::str::from_utf8(&bytes[range.start as usize..range.end as usize])
                            .unwrap();
                    renames.push((*range, format!("{orig}: {name}"), "ref(shorthand)"));
                } else {
                    renames.push((*range, name.clone(), "ref"));
                }
            }
        }
    }

    // Rebuild the file left-to-right, splicing in fresh names at each range;
    // sorting by start makes the single forward pass below correct.
    renames.sort_by_key(|(r, _, _)| r.start);
    for w in renames.windows(2) {
        if w[0].0.end > w[1].0.start {
            let lo = w[0].0.start.saturating_sub(30) as usize;
            let hi = (w[1].0.end as usize + 30).min(bytes.len());
            panic!(
                "overlapping rename ranges {:?} (name={}, origin={}) and {:?} (name={}, origin={}) near: {:?}",
                w[0].0, w[0].1, w[0].2,
                w[1].0, w[1].1, w[1].2,
                String::from_utf8_lossy(&bytes[lo..hi]),
            );
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    for (range, name, _) in &renames {
        out.extend_from_slice(&bytes[cursor..range.start as usize]);
        out.extend_from_slice(name.as_bytes());
        cursor = range.end as usize;
    }
    out.extend_from_slice(&bytes[cursor..]);
    String::from_utf8(out).unwrap()
}

fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            visit(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("mjs") {
            out.push(p);
        }
    }
}

fn node() -> Command {
    let mut cmd = Command::new("node");
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd
}

#[test]
#[ignore]
fn o9_js_alpha_renamed_tree_still_loads() {
    let lang = JsLang;
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    // The repo's whole `.mjs` tree: the harness plugin and the fake ACP
    // agent. (There is no `tests/*.mjs` suite; when one appears under
    // these roots the walk below picks it up.)
    let mut files = Vec::new();
    visit(&workspace_root.join("harness"), &mut files);
    visit(&workspace_root.join("crates/svc-agent/tests"), &mut files);
    assert!(!files.is_empty(), "expected at least one .mjs file");

    let scratch = std::env::temp_dir().join(format!("svc-o9-js-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let mut next_id = 0u32;
    let mut rels = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let renamed = alpha_rename_file(&src, &lang, &mut next_id);
        let rel = path.strip_prefix(workspace_root).unwrap();
        let dest = scratch.join(rel);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, renamed).unwrap();
        rels.push(rel.to_path_buf());
    }
    assert!(next_id > 0, "expected at least one JS Value local to rename");
    eprintln!("o9-js: renamed {next_id} locals across {} files in {}", files.len(), scratch.display());

    // 1. Every renamed file still parses.
    for rel in &rels {
        let output = node()
            .args(["--check", scratch.join(rel).to_str().unwrap()])
            .output()
            .expect("node must be on PATH for the JS oracle");
        assert!(
            output.status.success(),
            "node --check failed on renamed {}:\n{}",
            rel.display(),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // 2. The renamed plugin still loads (catches broken export bindings
    // and duplicate fresh names, which parse fine but fail at link time).
    let tools = scratch.join("harness/svc-tools.mjs");
    if tools.exists() {
        let output = node()
            .args(["--input-type=module", "-e", &format!("await import({:?})", tools)])
            .output()
            .expect("node must be on PATH for the JS oracle");
        assert!(
            output.status.success(),
            "renamed harness/svc-tools.mjs failed to import:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // 3. The renamed fake agent still runs: closed stdin ends the readline
    // loop, so it should print its banner on stderr and exit 0.
    let fake = scratch.join("crates/svc-agent/tests/fake_agent.mjs");
    if fake.exists() {
        let output = node()
            .arg(fake.to_str().unwrap())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("node must be on PATH for the JS oracle");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stderr.contains("fake agent up"),
            "renamed fake_agent.mjs misbehaves: status={} stderr={stderr}",
            output.status,
        );
    }
}
