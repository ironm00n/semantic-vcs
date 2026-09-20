//! `svc history export|import` at the library level, for scripts that run before the CLI
//! arms exist: `cargo run -q -p svc-repo --example history -- export [SINCE] [OUT]` writes
//! the bundle (stdout when OUT is absent); `… -- import FILE` replays one into the store
//! above the working directory and prints the report as JSON.

use std::path::Path;

use svc_core::OpIx;
use svc_repo::Repo;
use svc_repo::bundle::{self, Bundle};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().expect("cwd");
    let repo = Repo::discover(&cwd, Repo::default_langs()).unwrap_or_else(|e| fail(&e.to_string()));
    match args.first().map(String::as_str) {
        Some("export") => {
            let since = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
            let until = args.get(3).and_then(|s| s.parse().ok()).map(OpIx);
            let b = bundle::export_range(&repo, OpIx(since), until).unwrap_or_else(|e| fail(&e.to_string()));
            let text = serde_json::to_string_pretty(&b).expect("json");
            match args.get(2) {
                Some(out) => {
                    std::fs::write(Path::new(out), text).unwrap_or_else(|e| fail(&e.to_string()));
                    println!("{{\"path\":{:?},\"ops\":{}}}", out, b.entries.len());
                }
                None => println!("{text}"),
            }
        }
        Some("import") => {
            let file = args.get(1).unwrap_or_else(|| fail("import FILE"));
            let text = std::fs::read_to_string(file).unwrap_or_else(|e| fail(&e.to_string()));
            let b: Bundle = serde_json::from_str(&text).unwrap_or_else(|e| fail(&e.to_string()));
            let r = bundle::import(&repo, &b).unwrap_or_else(|e| fail(&e.to_string()));
            println!("{}", serde_json::to_string(&r).expect("json"));
        }
        _ => fail("usage: history export [SINCE] [OUT] | history import FILE"),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("history: {msg}");
    std::process::exit(1)
}
