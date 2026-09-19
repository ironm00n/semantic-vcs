use std::{env, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use svc_core::engine::{
    add_def, classify_def, delete, diff as diff_snapshots, edit_def, extract_hoist, inline,
    move_def, relocate, rename, render_entity, show,
};
use svc_core::{EntityId, Intent, Op, OpIx, RelPath, Snapshot, SnapshotId};
use svc_repo::{
    Repo, Take, blame, branch, changeset_begin, changeset_end, changeset_status,
    changesets, checkout, conflicts as list_conflicts, describe, edit, evolog, heads, log,
    merge as merge_repo, new, op_log, op_restore, resolve as resolve_conflict,
    resolve_entity, status, undo,
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
    Checkout { snapshot: String }, Render, Rename(RenameArgs), Move(MoveArgs),
    Relocate(RelocateArgs), Extract(ExtractArgs), Inline(EntityArg), AddDef(AddDefArgs),
    Delete(DeleteArgs), EditDef(EditDefArgs), Classify(ClassifyArgs), Agent { task: String }, Tui,
}

#[derive(Subcommand)] enum OpCommand { Log, Restore { index: u64 } }
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
#[derive(Args)] struct AddDefArgs { #[arg(long)] id: Option<String>, #[arg(long)] parent: Option<String>, #[arg(long)] ordinal: u32, #[arg(long)] definition: String, #[arg(long)] intent: String }
#[derive(Args)] struct DeleteArgs { #[arg(long)] entity: String, #[arg(long)] intent: String }
#[derive(Args)] struct EditDefArgs { #[arg(long)] entity: String, #[arg(long)] definition: String, #[arg(long)] intent: String }
#[derive(Args)] struct ClassifyArgs { #[arg(long)] entity: String, #[arg(long)] definition: String }

fn main() -> ExitCode {
    let cli = Cli::parse();
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
    match run(&cli) {
        Ok(value) => { println!("{}", if cli.json { value.to_string() } else { serde_json::to_string_pretty(&value).unwrap() }); ExitCode::SUCCESS }
        Err(error) => { if cli.json { eprintln!("{}", json!({"error": error})) } else { eprintln!("svc: {error}") }; ExitCode::FAILURE }
    }
}

fn run(cli: &Cli) -> Result<Value, String> {
    if matches!(cli.command, Command::Init) {
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        let repo = Repo::init(&cwd, Repo::default_langs()).map_err(|e| e.to_string())?;
        return Ok(json!({"root": repo.root_dir(), "change": repo.current_change().map_err(|e| e.to_string())?}));
    }
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let repo = Repo::discover(&cwd, Repo::default_langs()).map_err(|e| e.to_string())?;
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
        Command::Changeset(ChangeSetCommand::Status) => value(changeset_status(&repo)),
        Command::Changeset(ChangeSetCommand::List) => value(changesets(&repo)),
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
        Command::Tui => Err("run the `svc-tui` binary from this repository".into()),
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
    Ok(json!({"id": id, "canonical": canonical}))
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
    let op = Op::Rename { id, new: args.new_name.clone() };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, rename(cur, id, &args.new_name)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn move_cmd(repo: &Repo, args: &MoveArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let parent = resolve_parent(repo, &args.new_parent)?;
    let op = Op::Move { id, parent, ordinal: args.ordinal };
    let m = repo
        .mutate(op, None, |repo, cur| repo.amend(cur, move_def(cur, id, parent, args.ordinal)?))
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
        .mutate(op, None, |repo, cur| repo.amend(cur, extract_hoist(cur, id, parent, ordinal)?))
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn inline_cmd(repo: &Repo, entity: &str) -> Result<Value, String> {
    let id = resolve_entity(repo, entity).map_err(|e| e.to_string())?;
    let m = repo
        .mutate(Op::Inline { id }, None, |repo, cur| {
            repo.amend(cur, inline(cur, repo.store(), id)?)
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
    let op = Op::AddDef {
        id,
        parent,
        ordinal: args.ordinal,
        definition: args.definition.clone(),
        intent: intent.clone(),
    };
    let m = repo
        .mutate(op, None, |repo, cur| {
            let next = add_def(
                repo.store(), repo.langs(), cur, id, parent, args.ordinal,
                args.definition.as_bytes(), intent,
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
    let observed = classify_def(
        repo.store(), repo.langs(), &current, id, args.definition.as_bytes(),
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
                repo.store(), repo.langs(), cur, id, args.definition.as_bytes(),
            )?;
            repo.amend(cur, next)
        })
        .map_err(|e| e.to_string())?;
    mutation_value(m)
}

fn classify_cmd(repo: &Repo, args: &ClassifyArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let current = repo.current().map_err(|e| e.to_string())?;
    let observed = classify_def(
        repo.store(), repo.langs(), &current, id, args.definition.as_bytes(),
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
        Command::Agent { .. } => "agent", Command::Tui => "tui", _ => unreachable!(),
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
            "feature",
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
