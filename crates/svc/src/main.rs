use std::{env, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use svc_core::EntityId;
use svc_repo::{Repo, blame, branch, describe, evolog, heads, log, new, op_log, undo};

#[derive(Parser)]
#[command(name = "svc", about = "Compiler-grade version control")]
struct Cli {
    #[arg(long, global = true)] json: bool,
    #[command(subcommand)] command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init, Status, Describe { message: String }, New, Branch { name: String }, Heads, Log,
    Evolog { change: String }, Show { entity: String }, ListDefs, ShowDef(EntityArg),
    Search { query: String }, Diff { a: String, b: String }, Blame(EntityArg),
    Merge { change: String }, Conflicts, Resolve { conflict: String }, Undo,
    #[command(subcommand)] Op(OpCommand),
    Checkout { snapshot: String }, Render, Rename(RenameArgs), Move(MoveArgs),
    Relocate(RelocateArgs), Extract(ExtractArgs), Inline(EntityArg), AddDef(AddDefArgs),
    Delete(DeleteArgs), EditDef(EditDefArgs), Classify(ClassifyArgs), Agent { task: String }, Tui,
}

#[derive(Subcommand)] enum OpCommand { Log }
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
        Command::Status => Ok(json!({"clean": repo.working_copy_clean().map_err(|e| e.to_string())?, "change": repo.current_change().map_err(|e| e.to_string())?})),
        Command::Describe { message } => value(describe(&repo, message)),
        Command::New => value(new(&repo)),
        Command::Branch { name } => value(branch(&repo, name)),
        Command::Heads => value(heads(&repo)),
        Command::Log => value(log(&repo, None)),
        Command::Evolog { change } => { let id = repo.resolve_change(change).map_err(|e| e.to_string())?; value(evolog(&repo, id)) }
        Command::Blame(arg) => value(blame(&repo, resolve_entity(&repo, &arg.entity)?)),
        Command::Undo => value(undo(&repo)),
        Command::Op(OpCommand::Log) => value(op_log(&repo)),
        Command::Render => { let snapshot = repo.current().map_err(|e| e.to_string())?; repo.render_to_disk(&snapshot).map_err(|e| e.to_string())?; Ok(json!({"rendered": true})) }
        Command::ListDefs => list_defs(&repo),
        Command::ShowDef(arg) => show_def(&repo, &arg.entity),
        Command::Show { entity } => show_def(&repo, entity),
        command => Err(format!("{} is not wired to the engine yet", command_name(command))),
    }
}

fn value<T: Serialize>(result: svc_core::Result<T>) -> Result<Value, String> {
    serde_json::to_value(result.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn resolve_entity(repo: &Repo, query: &str) -> Result<EntityId, String> {
    repo.current().map_err(|e| e.to_string())?.entities.into_iter()
        .find(|(id, entity)| entity.name == query || id.to_string() == query || id.short() == query)
        .map(|(id, _)| id).ok_or_else(|| format!("no such definition {query}"))
}

fn list_defs(repo: &Repo) -> Result<Value, String> {
    let snapshot = repo.current().map_err(|e| e.to_string())?;
    Ok(json!({"definitions": snapshot.entities.into_iter().map(|(id, entity)| json!({
        "id": id, "name": entity.name, "kind": entity.kind, "file": entity.file,
        "parent": entity.parent, "ordinal": entity.ordinal
    })).collect::<Vec<_>>() }))
}

fn show_def(repo: &Repo, query: &str) -> Result<Value, String> {
    let id = resolve_entity(repo, query)?;
    let entity = repo.current().map_err(|e| e.to_string())?.entities.remove(&id).unwrap();
    let bytes = repo.store().get_bytes_blob(entity.bytes).map_err(|e| e.to_string())?;
    let content = repo.store().get_content(entity.content).map_err(|e| e.to_string())?;
    Ok(json!({"id": id, "entity": entity, "bytes": bytes, "content": content}))
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Search { .. } => "search", Command::Diff { .. } => "diff", Command::Merge { .. } => "merge",
        Command::Conflicts => "conflicts", Command::Resolve { .. } => "resolve", Command::Checkout { .. } => "checkout",
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
    }
}
