use std::{env, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use svc_core::engine::{
    add_def, classify_def, delete, diff, edit_def, extract_hoist, inline, move_def, relocate,
    rename, show,
};
use svc_core::{EntityId, Intent, Op, OpIx, RelPath, SnapshotId};
use svc_repo::{
    Mutation, Repo, Take, blame, branch, changeset_begin, changeset_end, changeset_status,
    changesets, checkout, conflicts as list_conflicts, describe, edit, evolog, heads, log,
    merge as merge_repo, new, op_log, op_restore, parse_entity_id, resolve as resolve_conflict,
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
#[derive(Args)] struct AddDefArgs { #[arg(long)] id: String, #[arg(long)] parent: Option<String>, #[arg(long)] ordinal: u32, #[arg(long)] definition: String, #[arg(long)] intent: String }
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
        Command::Show { entity } => show_def(&repo, entity),
        Command::Search { query } => search_defs(&repo, query),
        Command::Diff { a, b } => diff_changes(&repo, a, b),
        Command::Rename(args) => op_rename(&repo, args),
        Command::Move(args) => op_move(&repo, args),
        Command::Relocate(args) => op_relocate(&repo, args),
        Command::Extract(args) => op_extract(&repo, args),
        Command::Inline(arg) => op_inline(&repo, &arg.entity),
        Command::AddDef(args) => op_add_def(&repo, args),
        Command::Delete(args) => op_delete(&repo, args),
        Command::EditDef(args) => op_edit_def(&repo, args),
        Command::Classify(args) => op_classify(&repo, args),
        Command::Tui => Err(
            "svc tui lives in crates/svc-tui on claude@; this binary does not host it yet".into(),
        ),
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

fn search_defs(repo: &Repo, query: &str) -> Result<Value, String> {
    let snap = repo.current().map_err(|e| e.to_string())?;
    let q = query.to_ascii_lowercase();
    let hits: Vec<_> = snap
        .entities
        .iter()
        .filter(|(_, e)| e.name.to_ascii_lowercase().contains(&q))
        .map(|(id, e)| json!({"id": id, "name": e.name, "kind": e.kind, "file": e.file}))
        .collect();
    Ok(json!({"matches": hits}))
}

fn diff_changes(repo: &Repo, a: &str, b: &str) -> Result<Value, String> {
    let ia = repo.resolve_change(a).map_err(|e| e.to_string())?;
    let ib = repo.resolve_change(b).map_err(|e| e.to_string())?;
    let sa = repo.store().get_snapshot(repo.store().head(ia).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let sb = repo.store().get_snapshot(repo.store().head(ib).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(json!({"deltas": diff(&sa, &sb)}))
}

fn mutation_json(repo: &Repo, m: Mutation) -> Result<Value, String> {
    Ok(json!({
        "ix": m.ix,
        "snapshot": m.snapshot,
        "change": repo.current_change().map_err(|e| e.to_string())?,
        "observed": m.entry.observed,
        "flagged": m.entry.flagged(),
    }))
}

fn op_rename(repo: &Repo, args: &RenameArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let new = args.new_name.clone();
    let m = repo
        .mutate(Op::Rename { id, new: new.clone() }, None, |repo, cur| {
            repo.amend(cur, rename(cur, id, &new)?)
        })
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_move(repo: &Repo, args: &MoveArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let parent = if args.new_parent.is_empty() {
        None
    } else {
        Some(resolve_entity(repo, &args.new_parent).map_err(|e| e.to_string())?)
    };
    let m = repo
        .mutate(
            Op::Move { id, parent, ordinal: args.ordinal },
            None,
            |repo, cur| repo.amend(cur, move_def(cur, id, parent, args.ordinal)?),
        )
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_relocate(repo: &Repo, args: &RelocateArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let file = RelPath::new(&args.file).map_err(|e| e.to_string())?;
    let m = repo
        .mutate(
            Op::Relocate { id, file: file.clone(), ordinal: args.ordinal },
            None,
            |repo, cur| repo.amend(cur, relocate(cur, id, file.clone(), args.ordinal)?),
        )
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_extract(repo: &Repo, args: &ExtractArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let parent = match &args.new_parent {
        Some(p) if !p.is_empty() => Some(resolve_entity(repo, p).map_err(|e| e.to_string())?),
        _ => None,
    };
    let ordinal = repo
        .current()
        .map_err(|e| e.to_string())?
        .entities
        .get(&id)
        .map(|r| r.ordinal)
        .unwrap_or(0);
    let m = repo
        .mutate(
            Op::Extract { id, new_parent: parent, ordinal },
            None,
            |repo, cur| repo.amend(cur, extract_hoist(cur, id, parent, ordinal)?),
        )
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_inline(repo: &Repo, entity: &str) -> Result<Value, String> {
    let id = resolve_entity(repo, entity).map_err(|e| e.to_string())?;
    let m = repo
        .mutate(Op::Inline { id }, None, |repo, cur| {
            repo.amend(cur, inline(cur, repo.store(), id)?)
        })
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_add_def(repo: &Repo, args: &AddDefArgs) -> Result<Value, String> {
    let id = parse_entity_id(&args.id).unwrap_or_else(EntityId::new);
    let parent = match &args.parent {
        Some(p) if !p.is_empty() => Some(resolve_entity(repo, p).map_err(|e| e.to_string())?),
        _ => None,
    };
    let definition = args.definition.clone();
    let intent = parse_intent(&args.intent);
    let m = repo
        .mutate(
            Op::AddDef {
                id,
                parent,
                ordinal: args.ordinal,
                definition: definition.clone(),
                intent: intent.clone(),
            },
            None,
            |repo, cur| {
                repo.amend(
                    cur,
                    add_def(
                        repo.store(),
                        repo.langs(),
                        cur,
                        id,
                        parent,
                        args.ordinal,
                        definition.as_bytes(),
                        intent.clone(),
                    )?,
                )
            },
        )
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_delete(repo: &Repo, args: &DeleteArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let intent = parse_intent(&args.intent);
    let m = repo
        .mutate(Op::Delete { id, intent }, None, |repo, cur| {
            repo.amend(cur, delete(cur, repo.store(), id)?)
        })
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_edit_def(repo: &Repo, args: &EditDefArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let definition = args.definition.clone();
    let intent = parse_intent(&args.intent);
    let cur = repo.current().map_err(|e| e.to_string())?;
    let (_, class) = edit_def(repo.store(), repo.langs(), &cur, id, definition.as_bytes())
        .map_err(|e| e.to_string())?;
    let m = repo
        .mutate(
            Op::EditDef {
                id,
                definition: definition.clone(),
                intent,
            },
            Some(class),
            |repo, cur| {
                let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
                repo.amend(cur, next)
            },
        )
        .map_err(|e| e.to_string())?;
    mutation_json(repo, m)
}

fn op_classify(repo: &Repo, args: &ClassifyArgs) -> Result<Value, String> {
    let id = resolve_entity(repo, &args.entity).map_err(|e| e.to_string())?;
    let snap = repo.current().map_err(|e| e.to_string())?;
    let class = classify_def(repo.store(), repo.langs(), &snap, id, args.definition.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(json!({"entity": id, "observed": class}))
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
    }
}
