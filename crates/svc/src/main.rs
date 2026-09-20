use std::{env, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use svc_core::engine::{
    add_def_at, classify_def, delete, diff as diff_snapshots, edit_def, extract_hoist, inline,
    move_def, relocate, rename, render_entity, resolve_add_def_file, show,
};
use svc_core::{EntityId, Intent, Op, OpIx, RelPath, Snapshot, SnapshotId};
use svc_repo::{
    Repo, Take, blame, branch, changeset_begin, changeset_end, changeset_status,
    changesets, checkout, conflicts as list_conflicts, describe, edit, evolog, heads, log,
    merge as merge_repo, new, op_log, op_restore, replay, resolve as resolve_conflict,
    resolve_entity, status, undo, untracked_mentions, workspace,
};

mod agent;

#[derive(Parser)]
#[command(name = "svc", about = "Compiler-grade version control")]
struct Cli {
    #[arg(long, global = true)] json: bool,
    #[command(subcommand)] command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init, Status, Describe { message: String }, New, Branch { name: String }, Edit { change: String }, Heads, Log,
    Evolog { change: String }, Show { entity: String }, ListDefs, ShowDef(EntityArg),
    Search { query: String }, Diff { a: String, b: String }, Blame(EntityArg),
    Merge { change: String }, Conflicts,
    Resolve { conflict: usize, #[arg(long)] take: String },
    Undo,
    #[command(subcommand)] Op(OpCommand),
    #[command(subcommand)] Changeset(ChangeSetCommand),
    /// The local forge (crates/svc-forge): `export` writes its catalog from this store.
    #[command(subcommand)] Forge(ForgeCommand),
    #[command(subcommand)] Workspace(WorkspaceCommand),
    Checkout { snapshot: String }, Render, Replay, Rename(RenameArgs), Move(MoveArgs),
    Relocate(RelocateArgs), Extract(ExtractArgs), Inline(EntityArg), AddDef(AddDefArgs),
    Delete(DeleteArgs), EditDef(EditDefArgs), Classify(ClassifyArgs), Agent { task: String },
    /// Open the review UI; with `--agent <task>`, run that task under dsh inside it (demo line 12).
    Tui { #[arg(long)] agent: Option<String>, #[arg(long)] wire_log: bool },
}

#[derive(Subcommand)] enum OpCommand { Log, Restore { index: u64 } }
#[derive(Subcommand)] enum ForgeCommand { Export { #[arg(long)] out: Option<std::path::PathBuf> } }
#[derive(Subcommand)]
enum WorkspaceCommand {
    Add { name: String, path: PathBuf, #[arg(long)] at: Option<String> },
    List,
    Forget { name: String },
    UpdateStale,
}
#[derive(Subcommand)]
enum ChangeSetCommand {
    Begin { name: String, #[arg(long, default_value = "refactor")] intent: String, #[arg(long)] force: bool },
    End,
    Status,
    List,
}
#[derive(Args)] struct EntityArg { #[arg(long)] entity: String }
#[derive(Args)] struct RenameArgs { #[arg(long)] entity: String, #[arg(long)] new_name: String }
#[derive(Args)] struct MoveArgs { #[arg(long)] entity: String, #[arg(long)] new_parent: String, #[arg(long)] ordinal: Option<u32> }
#[derive(Args)] struct RelocateArgs { #[arg(long)] entity: String, #[arg(long)] file: String, #[arg(long)] ordinal: u32 }
#[derive(Args)] struct ExtractArgs { #[arg(long)] entity: String, #[arg(long)] new_parent: Option<String> }
#[derive(Args)] struct AddDefArgs { #[arg(long)] id: Option<String>, #[arg(long)] parent: Option<String>, #[arg(long)] file: Option<String>, #[arg(long)] ordinal: u32, #[arg(long)] definition: String, #[arg(long)] intent: String }
#[derive(Args)] struct DeleteArgs { #[arg(long)] entity: String, #[arg(long)] intent: String }
#[derive(Args)] struct EditDefArgs { #[arg(long)] entity: String, #[arg(long)] definition: String, #[arg(long)] intent: String }
#[derive(Args)] struct ClassifyArgs { #[arg(long)] entity: String, #[arg(long)] definition: String }

fn main() -> ExitCode {
    // `svc list-defs --json | head` must not panic with "failed printing to stdout: Broken
    // pipe": restore SIGPIPE's default so a closed pipe ends the process quietly, as it
    // does for every other CLI tool.
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        unsafe { signal(13 /* SIGPIPE */, 0 /* SIG_DFL */) };
    }
    let cli = Cli::parse();
    if let Command::Tui { agent: task, wire_log } = &cli.command {
        let cwd = match env::current_dir().and_then(|path| path.canonicalize()) {
            Ok(cwd) => cwd,
            Err(error) => {
                eprintln!("svc: could not resolve repository root: {error}");
                return ExitCode::FAILURE;
            }
        };
        let root = Repo::find_root(&cwd).unwrap_or(cwd);
        let svc_bin = match env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("svc: could not locate its executable: {error}");
                return ExitCode::FAILURE;
            }
        };
        // Same launch as `svc agent`: pinned dsh, runtime overlay, SVC_BIN; the TUI opens
        // and closes the run's changeset itself and answers asks from its queue.
        // SVC_AGENT_COMMAND="node demo/replay-agent.mjs <recording>" hosts a scripted ACP
        // agent instead, for rehearsing line 12 without a model key.
        let agent = match task {
            None => None,
            Some(task) => match env::var("SVC_AGENT_COMMAND") {
                Ok(command) => {
                    let mut words = command.split_whitespace().map(str::to_string);
                    let Some(program) = words.next() else {
                        eprintln!("svc: SVC_AGENT_COMMAND is empty");
                        return ExitCode::FAILURE;
                    };
                    let mut config = svc_agent::AgentConfig::command(program, words.collect(), &root);
                    config.env.insert("SVC_BIN".into(), svc_bin.display().to_string());
                    Some((config, task.clone()))
                }
                Err(_) => {
                    if !agent::has_model_credentials() {
                        eprintln!("svc: no model credential found; set OPENROUTER_API_KEY, DEEPSEEK_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY, or XAI_API_KEY");
                        return ExitCode::FAILURE;
                    }
                    match agent::runtime_overlay(&root) {
                        Ok(overlay) => Some((svc_agent::AgentConfig::dsh(&root, &overlay, &svc_bin), task.clone())),
                        Err(error) => {
                            eprintln!("svc: {error}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            },
        };
        return match svc_tui::run(svc_tui::TuiOptions {
            svc_bin,
            root,
            agent,
            wire_log: *wire_log,
        }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("svc: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if let Command::Agent { task } = &cli.command {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("svc: could not start async runtime: {error}");
                return ExitCode::FAILURE;
            }
        };
        return match runtime.block_on(agent::run(task)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("svc: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if !cli.json {
        if let Some(result) = run_text(&cli) {
            return match result {
                Ok(text) => { println!("{text}"); ExitCode::SUCCESS }
                Err(error) => { eprintln!("svc: {error}"); ExitCode::FAILURE }
            };
        }
    }
    match run(&cli) {
        Ok(value) => { println!("{}", if cli.json { value.to_string() } else { serde_json::to_string_pretty(&value).unwrap() }); ExitCode::SUCCESS }
        Err(error) => { if cli.json { eprintln!("{}", json!({"error": error})) } else { eprintln!("svc: {error}") }; ExitCode::FAILURE }
    }
}

/// The sentence form of the verbs whose output people read at the expo (design §8: `--json`
/// is the machine form). `None` for every other verb, which then takes the JSON path.
fn run_text(cli: &Cli) -> Option<Result<String, String>> {
    use svc_repo::text;
    let cwd = env::current_dir().ok()?;
    let repo = Repo::discover(&cwd, Repo::default_langs()).ok()?;
    let snap = repo.current().ok()?;
    let out = match &cli.command {
        Command::Status => status(&repo).and_then(|s| repo.current().map(|snap| text::status(&snap, &s))),
        Command::Log => log(&repo, None).map(|l| text::log(&snap, &l)),
        Command::Op(OpCommand::Log) => op_log(&repo).map(|l| text::log(&snap, &l)),
        Command::Forge(ForgeCommand::Export { out }) => svc_repo::forge::export(&repo, out.as_deref())
            .map(|p| format!("wrote {} — serve it with: cargo run -p svc-forge -- --catalog {}", p.display(), p.display())),
        Command::Heads => heads(&repo).map(|h| text::heads(&h)),
        Command::Evolog { change } => repo
            .resolve_change(change)
            .and_then(|id| evolog(&repo, id))
            .map(|e| text::evolog(&e)),
        Command::Blame(arg) => resolve_entity(&repo, &arg.entity)
            .and_then(|id| blame(&repo, id))
            .map(|b| text::blame(&snap, &b)),
        Command::Conflicts => list_conflicts(&repo).map(|c| text::conflicts_named(&snap, repo.store(), repo.root_dir(), &c)),
        Command::Replay => replay(&repo).and_then(|r| {
            if r.ok() {
                Ok(format!("replayed {} operations: clean", r.ops))
            } else {
                Err(svc_core::Error::Other(format!(
                    "replay diverged at {:?}",
                    r.diverged_at
                )))
            }
        }),
        Command::Workspace(WorkspaceCommand::List) => {
            return Some(
                workspace::list(&repo)
                    .map(|rows| {
                        rows.iter()
                            .map(|row| {
                                let mark = if row.current { "@" } else { " " };
                                let change = row
                                    .change
                                    .as_ref()
                                    .map(|id| format!("  change⟨{}⟩", id.short()))
                                    .unwrap_or_default();
                                format!("{mark} {:<8} {}{change}", row.name, row.path.display())
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .map_err(|e| e.to_string()),
            );
        }
        Command::Merge { change } => merge_repo(&repo, change)
            .and_then(|m| repo.current().map(|s| text::merge(&s, repo.store(), repo.root_dir(), &m))),
        Command::Show { entity } => {
            // demo line 3: the canonical stream — `$n` local slots, `#name⟨hash⟩` refs.
            return Some(show_canonical(&repo, entity).map(|v| {
                format!("{entity}⟨{}⟩\n{}", v["short"].as_str().unwrap_or(""), v["canonical"].as_str().unwrap_or(""))
            }));
        }
        Command::New
        | Command::Branch { .. }
        | Command::Describe { .. }
        | Command::Undo
        | Command::Move(_)
        | Command::Relocate(_)
        | Command::Extract(_)
        | Command::Inline(_)
        | Command::AddDef(_)
        | Command::Delete(_)
        | Command::EditDef(_) => {
            // Run the verb, then read back what it recorded: the newest op-log line.
            return Some(run_with(cli, &repo).and_then(|_| {
                let snap = repo.current().map_err(|e| e.to_string())?;
                let entries = op_log(&repo).map_err(|e| e.to_string())?;
                Ok(entries.first().map(|e| text::op(&snap, e)).unwrap_or_default())
            }));
        }
        Command::Rename(args) => {
            return Some(rename_cmd(&repo, args).map(|v| {
                let from = v["renamed"]["from"].as_str().unwrap_or("?").to_string();
                let calls = v["untracked_mentions"]["method_calls"].as_u64().unwrap_or(0);
                let other = v["untracked_mentions"]["other"].as_u64().unwrap_or(0);
                let plural = |n: u64, s: &str| if n == 1 { s.to_string() } else { format!("{s}s") };
                let mut line = format!("renamed {from} → {}", args.new_name);
                if calls > 0 {
                    line.push_str(&format!(
                        "\n{calls} method {} `.{from}(…)` left unchanged: receiver types are not resolved",
                        plural(calls, "call")
                    ));
                }
                if other > 0 {
                    line.push_str(&format!(
                        "\n{other} other {} of `{from}` left unchanged (strings, comments, unrelated bindings)",
                        plural(other, "mention")
                    ));
                }
                line
            }));
        }
        _ => return None,
    };
    Some(out.map_err(|e| e.to_string()))
}

fn run(cli: &Cli) -> Result<Value, String> {
    if matches!(cli.command, Command::Init) {
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        let repo = Repo::init(&cwd, Repo::default_langs()).map_err(|e| e.to_string())?;
        return Ok(json!({"root": repo.root_dir(), "change": repo.current_change().map_err(|e| e.to_string())?}));
    }
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let repo = Repo::discover(&cwd, Repo::default_langs()).map_err(|e| e.to_string())?;
    run_with(cli, &repo)
}

/// The verb dispatch over an already-open repository (redb admits one opener per process).
fn run_with(cli: &Cli, repo: &Repo) -> Result<Value, String> {
    let repo = repo;
    match &cli.command {
        Command::Status => value(status(&repo)),
        Command::Describe { message } => value(describe(&repo, message)),
        Command::New => value(new(&repo)),
        Command::Branch { name } => value(branch(&repo, name)),
        Command::Edit { change } => value(edit(&repo, change)),
        Command::Heads => value(heads(&repo)),
        Command::Log => value(log(&repo, None)),
        Command::Evolog { change } => { let id = repo.resolve_change(change).map_err(|e| e.to_string())?; value(evolog(&repo, id)) }
        Command::Blame(arg) => value(blame(&repo, resolve_entity(&repo, &arg.entity).map_err(|e| e.to_string())?)),
        Command::Undo => value(undo(&repo)),
        Command::Merge { change } => value(merge_repo(&repo, change)),
        Command::Conflicts => value(list_conflicts(&repo)),
        Command::Resolve { conflict, take } => {
            value(resolve_conflict(&repo, *conflict, parse_take(take)?))
        }
        Command::Op(OpCommand::Log) => value(op_log(&repo)),
        Command::Op(OpCommand::Restore { index }) => value(op_restore(&repo, OpIx(*index))),
        Command::Changeset(ChangeSetCommand::Begin { name, intent, force }) => {
            value(changeset_begin(&repo, name, parse_intent(intent), None, *force))
        }
        Command::Changeset(ChangeSetCommand::End) => value(changeset_end(&repo)),
        Command::Forge(ForgeCommand::Export { out }) => svc_repo::forge::export(&repo, out.as_deref()).map(|p| json!({"path": p})).map_err(|e| e.to_string()),
        Command::Changeset(ChangeSetCommand::Status) => value(changeset_status(&repo)),
        Command::Changeset(ChangeSetCommand::List) => value(changesets(&repo)),
        Command::Workspace(WorkspaceCommand::Add { name, path, at }) => {
            let change = at
                .as_deref()
                .map(|value| repo.resolve_change(value).map_err(|e| e.to_string()))
                .transpose()?;
            value(workspace::add(&repo, name, path, change))
        }
        Command::Workspace(WorkspaceCommand::List) => value(workspace::list(&repo)),
        Command::Workspace(WorkspaceCommand::Forget { name }) => {
            value(workspace::forget(&repo, name))
        }
        Command::Workspace(WorkspaceCommand::UpdateStale) => value(workspace::update_stale(&repo)),
        Command::Replay => {
            let report = replay(&repo).map_err(|e| e.to_string())?;
            if !report.ok() {
                return Err(format!("replay diverged at {:?}", report.diverged_at));
            }
            value(Ok(report))
        }
        Command::Checkout { snapshot } => {
            let id = snapshot.parse::<SnapshotId>().map_err(|e| e.to_string())?;
            value(checkout(&repo, id))
        }
        Command::Render => { let snapshot = repo.current().map_err(|e| e.to_string())?; repo.render_to_disk(&snapshot).map_err(|e| e.to_string())?; Ok(json!({"rendered": true})) }
        Command::ListDefs => list_defs(&repo),
        Command::ShowDef(arg) => show_def(&repo, &arg.entity),
        Command::Show { entity } => show_canonical(&repo, entity),
        Command::Search { query } => search(&repo, query),
        Command::Diff { a, b } => diff(&repo, a, b),
        Command::Rename(args) => rename_cmd(&repo, args),
        Command::Move(args) => move_cmd(&repo, args),
        Command::Relocate(args) => relocate_cmd(&repo, args),
        Command::Extract(args) => extract_cmd(&repo, args),
        Command::Inline(arg) => inline_cmd(&repo, &arg.entity),
        Command::AddDef(args) => add_def_cmd(&repo, args),
        Command::Delete(args) => delete_cmd(&repo, args),
        Command::EditDef(args) => edit_def_cmd(&repo, args),
        Command::Classify(args) => classify_cmd(&repo, args),
        Command::Tui { .. } => unreachable!("handled before the repository opens"),
        command => Err(format!("{} is not wired to the engine yet", command_name(command))),
    }
}

fn value<T: Serialize>(result: svc_core::Result<T>) -> Result<Value, String> {
    serde_json::to_value(result.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn parse_intent(value: &str) -> Intent {
    match value {
        "refactor" => Intent::Refactor,
        "fix" => Intent::Fix,
        "feature" => Intent::Feature,
        "docs" => Intent::Docs,
        other => Intent::Other(other.to_owned()),
    }
}

fn parse_take(value: &str) -> Result<Take, String> {
    match value {
        "a" => Ok(Take::A),
        "b" => Ok(Take::B),
        "base" => Ok(Take::Base),
        _ => Err("--take must be a, b, or base".into()),
    }
}

fn definition_bytes(definition: &str) -> Vec<u8> {
    let mut bytes = definition.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes
}

fn list_defs(repo: &Repo) -> Result<Value, String> {
    let snapshot = repo.current().map_err(|e| e.to_string())?;
    Ok(json!({"definitions": snapshot.entities.into_iter().map(|(id, entity)| json!({
        "id": id, "name": entity.name, "kind": entity.kind, "file": entity.file,
        "parent": entity.parent, "ordinal": entity.ordinal
    })).collect::<Vec<_>>() }))
}

fn show_def(repo: &Repo, query: &str) -> Result<Value, String> {
    let snap = repo.current().map_err(|e| e.to_string())?;
    let id = resolve_entity(repo, query).map_err(|e| e.to_string())?;
    let canonical = show(repo.store(), &snap, id).map_err(|e| e.to_string())?;
    let entity = snap.entities.get(&id).cloned().ok_or_else(|| format!("missing {query}"))?;
    let bytes = repo.store().get_bytes_blob(entity.bytes).map_err(|e| e.to_string())?;
    let content = repo.store().get_content(entity.content).map_err(|e| e.to_string())?;
    Ok(json!({"id": id, "entity": entity, "canonical": canonical, "bytes": bytes, "content": content}))
}

fn show_canonical(repo: &Repo, query: &str) -> Result<Value, String> {
    let id = resolve_entity(repo, query).map_err(|e| e.to_string())?;
    let snapshot = repo.current().map_err(|e| e.to_string())?;
    let canonical = show(repo.store(), &snapshot, id).map_err(|e| e.to_string())?;
    Ok(json!({"id": id, "short": id.short(), "canonical": canonical}))
}

fn mutation_value(m: svc_repo::Mutation) -> Result<Value, String> {
    Ok(json!({
        "op": m.ix,
        "snapshot": m.snapshot,
        "observed": m.entry.observed,
        "flagged": m.entry.flagged(),
        "closed_stale_changeset": m.closed_stale_changeset,
    }))
}

fn search(repo: &Repo, query: &str) -> Result<Value, String> {
    let needle = query.to_ascii_lowercase();
    let snapshot = repo.current().map_err(|e| e.to_string())?;
    let matches = snapshot
        .entities
        .iter()
        .filter(|(id, entity)| {
            id.to_string().to_ascii_lowercase().contains(&needle)
                || entity.name.to_ascii_lowercase().contains(&needle)
                || entity.file.as_str().to_ascii_lowercase().contains(&needle)
                || format!("{:?}", entity.kind).to_ascii_lowercase().contains(&needle)
                || render_entity(&snapshot, repo.store(), **id, false)
                    .ok()
                    .is_some_and(|(src, _)| {
                        String::from_utf8_lossy(&src).to_ascii_lowercase().contains(&needle)
                    })
        })
        .map(|(id, entity)| json!({
            "id": id, "name": entity.name, "kind": entity.kind, "file": entity.file,
            "parent": entity.parent, "ordinal": entity.ordinal,
        }))
        .collect::<Vec<_>>();
    Ok(json!({"matches": matches}))
}

fn resolve_snapshot(repo: &Repo, spec: &str) -> Result<Snapshot, String> {
    if let Ok(id) = spec.parse::<SnapshotId>() {
        if let Ok(snapshot) = repo.store().get_snapshot(id) {
            return Ok(snapshot);
        }
    }
    let change = repo.resolve_change(spec).map_err(|e| e.to_string())?;
    let id = repo
        .store()
        .head(change)
        .map_err(|e| e.to_string())?;
    repo.store().get_snapshot(id).map_err(|e| e.to_string())
}

fn diff(repo: &Repo, a: &str, b: &str) -> Result<Value, String> {
    let a = resolve_snapshot(repo, a)?;
    let b = resolve_snapshot(repo, b)?;
    Ok(json!({"deltas": diff_snapshots(&a, &b)}))
}

fn resolve_parent(repo: &Repo, parent: &str) -> Result<Option<EntityId>, String> {
    match parent {
        "root" | "none" | "-" => Ok(None),
        value => resolve_entity(repo, value).map(Some).map_err(|e| e.to_string()),
    }
}

fn rename_cmd(repo: &Repo, args: &RenameArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let old = repo.current().ok().and_then(|s| s.entities.get(&id).map(|r| r.name.clone()));
    let op = Op::Rename { id, new: args.new_name.clone() };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, rename(cur, id, &args.new_name)?))
        .map_err(|e| e.to_string())?;
    // Mentions of the old name svc did not resolve (method calls on typed receivers,
    // strings, comments) are left as they were; say how many rather than hide it.
    let untracked = match &old {
        Some(old) if old != &args.new_name => untracked_mentions(repo, old).unwrap_or_default(),
        _ => Default::default(),
    };
    let mut v = mutation_value(m)?;
    v["renamed"] = json!({ "from": old, "to": args.new_name });
    v["untracked_mentions"] = json!(untracked);
    Ok(v)
}

fn move_cmd(repo: &Repo, args: &MoveArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let parent = resolve_parent(repo, &args.new_parent)?;
    let op = Op::Move { id, parent, ordinal: args.ordinal };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, move_def(repo.store(), cur, id, parent, args.ordinal)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn relocate_cmd(repo: &Repo, args: &RelocateArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let file = RelPath::new(args.file.clone()).map_err(|e| e.to_string())?;
    let op = Op::Relocate { id, file: file.clone(), ordinal: args.ordinal };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, relocate(cur, id, file, args.ordinal)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn extract_cmd(repo: &Repo, args: &ExtractArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let parent = args
        .new_parent
        .as_deref()
        .map(|p| resolve_parent(repo, p))
        .transpose()?
        .flatten();
    let ordinal = repo
        .current()
        .map_err(|e| e.to_string())?
        .entities
        .get(&id)
        .map(|e| e.ordinal)
        .ok_or_else(|| format!("no such entity: {}", args.entity))?;
    let op = Op::Extract { id, new_parent: parent, ordinal };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, extract_hoist(repo.store(), cur, id, parent, ordinal)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn inline_cmd(repo: &Repo, entity: &str) -> Result<Value, String> {
    let id = resolve_entity(repo, entity).map_err(|e| e.to_string())?;
    let m = repo
        .mutate(Op::Inline { id }, None, |repo, cur| {
            repo.amend(cur, inline(repo.store(), repo.langs(), cur, id)?)
        })
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn add_def_cmd(repo: &Repo, args: &AddDefArgs) -> Result<Value, String> {
    let id: EntityId = match &args.id {
        Some(id) => serde_json::from_value(Value::String(id.clone()))
            .map_err(|e| format!("invalid entity id: {e}"))?,
        None => EntityId::new(),
    };
    let parent = args
        .parent
        .as_deref()
        .map(|p| resolve_parent(repo, p))
        .transpose()?
        .flatten();
    let intent = parse_intent(&args.intent);
    let definition = definition_bytes(&args.definition);
    let file = match args.file.as_deref() {
        Some(s) => Some(RelPath::new(s).map_err(|p| format!("invalid --file {p}"))?),
        None => None,
    };
    let current = repo.current().map_err(|e| e.to_string())?;
    let file = resolve_add_def_file(&current, repo.langs(), parent, file)
        .map_err(|e| e.to_string())?;
    let op = Op::AddDef {
        id,
        parent,
        ordinal: args.ordinal,
        definition: args.definition.clone(),
        intent: intent.clone(),
        file: Some(file.clone()),
    };
    let m = repo
        .mutate(op, None, |repo, cur| {
            let next = add_def_at(
                repo.store(), repo.langs(), cur, id, parent, Some(file.clone()),
                args.ordinal, &definition, intent,
            )?;
            repo.amend(cur, next)
        })
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn delete_cmd(repo: &Repo, args: &DeleteArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let intent = parse_intent(&args.intent);
    let op = Op::Delete { id, intent };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, delete(cur, repo.store(), id)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn edit_def_cmd(repo: &Repo, args: &EditDefArgs) -> Result<Value, String> {
    repo.absorb().map_err(|e| e.to_string())?;
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let current = repo.current().map_err(|e| e.to_string())?;
    let definition = definition_bytes(&args.definition);
    let observed = classify_def(
        repo.store(), repo.langs(), &current, id, &definition,
    )
    .map_err(|e| e.to_string())?;
    let intent = parse_intent(&args.intent);
    let op = Op::EditDef {
        id,
        definition: args.definition.clone(),
        intent,
    };
    let m = repo
        .mutate(op, Some(observed), |repo, cur| {
            let (next, _) = edit_def(
                repo.store(), repo.langs(), cur, id, &definition,
            )?;
            repo.amend(cur, next)
        })
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn classify_cmd(repo: &Repo, args: &ClassifyArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let current = repo.current().map_err(|e| e.to_string())?;
    let definition = definition_bytes(&args.definition);
    let observed = classify_def(
        repo.store(), repo.langs(), &current, id, &definition,
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"observed": observed, "committed": false}))
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Search { .. } => "search", Command::Diff { .. } => "diff",
        Command::Rename(_) => "rename", Command::Move(_) => "move", Command::Relocate(_) => "relocate",
        Command::Extract(_) => "extract", Command::Inline(_) => "inline", Command::AddDef(_) => "add-def",
        Command::Delete(_) => "delete", Command::EditDef(_) => "edit-def", Command::Classify(_) => "classify",
        Command::Agent { .. } => "agent", Command::Tui { .. } => "tui", Command::Workspace(_) => "workspace", Command::Replay => "replay", _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn harness_shapes_parse() {
        Cli::try_parse_from(["svc", "rename", "--entity", "parse", "--new-name", "parse_config", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "edit-def", "--entity", "load", "--definition", "fn load() {}", "--intent", "refactor", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "list-defs", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "merge", "feature", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "resolve", "0", "--take", "b", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "edit", "feature", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "op", "restore", "7", "--json"]).unwrap();
        Cli::try_parse_from([
            "svc", "workspace", "add", "agent", "/tmp/agent", "--at", "main", "--json",
        ])
        .unwrap();
        Cli::try_parse_from(["svc", "workspace", "list", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "workspace", "forget", "agent", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "replay", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "workspace", "update-stale", "--json"]).unwrap();
        Cli::try_parse_from(["svc", "move", "--entity", "parse", "--new-parent", "root"])
            .unwrap();
        Cli::try_parse_from([
            "svc", "relocate", "--entity", "parse", "--file", "src/parse.rs", "--ordinal", "0",
        ])
        .unwrap();
        Cli::try_parse_from([
            "svc", "add-def", "--id", "018f0000-0000-7000-8000-000000000001",
            "--ordinal", "9", "--definition", "fn added() {}", "--intent", "feature",
        ])
        .unwrap();
        Cli::try_parse_from([
            "svc", "add-def", "--ordinal", "9", "--definition", "fn added() {}", "--intent",
            "feature", "--file", "src/extra.rs",
        ])
        .unwrap();
        Cli::try_parse_from([
            "svc", "classify", "--entity", "validate", "--definition", "fn validate() {}",
        ])
        .unwrap();
        Cli::try_parse_from(["svc", "search", "parse"]).unwrap();
        Cli::try_parse_from(["svc", "diff", "main", "feature"]).unwrap();
    }
}
