//! Standalone launcher; `svc tui` in the main binary calls `svc_tui::run` the same way.
//!
//!   svc-tui [--svc PATH] [--root DIR] [--agent "task"] [--overlay FILE] [--wire-log]

use std::path::PathBuf;

use svc_agent::AgentConfig;
use svc_tui::{TuiOptions, run};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut svc_bin: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    let mut task: Option<String> = None;
    let mut overlay: Option<PathBuf> = None;
    let mut wire_log = false;
    let mut agent_cmd: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--svc" => svc_bin = args.next().map(PathBuf::from),
            "--root" => root = args.next().map(PathBuf::from),
            "--agent" => task = args.next(),
            "--overlay" => overlay = args.next().map(PathBuf::from),
            "--wire-log" => wire_log = true,
            "--agent-cmd" => agent_cmd = args.next(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(root) = root
        .or_else(|| std::env::current_dir().ok())
        .and_then(|p| p.canonicalize().ok())
    else {
        eprintln!("svc-tui: the checkout directory does not exist or cannot be read (pass --root <dir>)");
        std::process::exit(2);
    };
    let svc_bin = svc_bin
        .or_else(|| std::env::var_os("SVC_BIN").map(PathBuf::from))
        .or_else(|| std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("svc"))));
    let Some(svc_bin) = svc_bin.filter(|p| p.is_file()) else {
        eprintln!("svc-tui: no svc binary beside this one; pass --svc <path> or set SVC_BIN");
        std::process::exit(2);
    };
    let agent = task.map(|task| {
        let cfg = match &agent_cmd {
            // Any ACP-speaking command, e.g. the tests' fake agent.
            Some(cmd) => {
                let mut parts = cmd.split_whitespace().map(str::to_string);
                let Some(exe) = parts.next() else {
                    eprintln!("svc-tui: --agent-cmd is empty; give the command that speaks ACP on stdio");
                    std::process::exit(2);
                };
                AgentConfig::command(exe, parts.collect(), &root)
            }
            None => AgentConfig::dsh(&root, &overlay.unwrap_or_else(|| root.join("harness/overlay.yml")), &svc_bin),
        };
        (cfg, task)
    });
    if let Err(e) = run(TuiOptions { svc_bin, root, agent, wire_log }) {
        eprintln!("svc-tui: {e}");
        std::process::exit(1);
    }
}
