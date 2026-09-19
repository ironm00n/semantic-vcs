//! Everything the TUI knows comes from `svc … --json` subprocesses (DECISIONS §11: the TUI
//! never holds the redb lock, so the agent's own `svc` calls can proceed while it runs).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use svc_core::{Conflict, Kind, Op};
use svc_repo::{BlameEntry, ConflictOut, OpOut};

#[derive(Clone, Debug, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub file: String,
    pub parent: Option<String>,
    pub ordinal: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct ListDefs {
    definitions: Vec<Definition>,
}

/// `svc show-def --json`. `text` is the utf-8 item; older binaries only had `bytes.src`.
#[derive(Clone, Debug, Deserialize)]
pub struct ShowDef {
    #[serde(default)]
    pub canonical: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    bytes: ShowDefBytes,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ShowDefBytes {
    #[serde(default)]
    src: Vec<u8>,
}

impl ShowDef {
    pub fn source(&self) -> String {
        if !self.text.is_empty() {
            return self.text.clone();
        }
        String::from_utf8_lossy(&self.bytes.src).into_owned()
    }
}

#[derive(Clone, Debug)]
pub struct Svc {
    pub bin: PathBuf,
    pub root: PathBuf,
}

impl Svc {
    pub fn new(bin: PathBuf, root: PathBuf) -> Self {
        Self { bin, root }
    }

    fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let out = Command::new(&self.bin)
            .args(args)
            .arg("--json")
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("spawn {}: {e}", self.bin.display()))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        serde_json::from_slice(&out.stdout).map_err(|e| {
            format!(
                "svc {}: bad json ({e}): {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout).chars().take(200).collect::<String>()
            )
        })
    }

    pub fn list_defs(&self) -> Result<Vec<Definition>, String> {
        self.json::<ListDefs>(&["list-defs"]).map(|l| l.definitions)
    }

    pub fn log(&self) -> Result<Vec<OpOut>, String> {
        self.json(&["log"])
    }

    pub fn blame(&self, entity_id: &str) -> Result<Vec<BlameEntry>, String> {
        self.json(&["blame", "--entity", entity_id])
    }

    /// Current source + canonical stream for the selected entity (right pane).
    pub fn show_def(&self, entity: &str) -> Result<ShowDef, String> {
        self.json(&["show-def", "--entity", entity])
    }

    pub fn conflicts(&self) -> Result<Vec<ConflictOut>, String> {
        self.json(&["conflicts"])
    }

    pub fn undo(&self) -> Result<serde_json::Value, String> {
        self.json(&["undo"])
    }

    /// Open the group every op of this agent run is stamped with (SPEC §5.6), so `u` and
    /// `svc undo` revert the run whole.
    pub fn changeset_begin(&self, name: &str) -> Result<serde_json::Value, String> {
        self.json(&["changeset", "begin", name])
    }

    pub fn changeset_end(&self) -> Result<serde_json::Value, String> {
        self.json(&["changeset", "end"])
    }

    pub fn root_exists(&self) -> bool {
        Path::new(&self.root).join(".svc").is_dir()
    }
}

/// One line per op, the way the events pane and the queue print it.
pub fn describe_op(op: &Op) -> String {
    match op {
        Op::Rename { new, .. } => format!("renamed → {new}"),
        Op::Move { parent, .. } => format!("moved → {}", parent.map(|p| p.short()).unwrap_or_else(|| "top level".into())),
        Op::Relocate { file, ordinal, .. } => format!("relocated → {file}#{ordinal}"),
        Op::Extract { .. } => "extracted (hoisted)".into(),
        Op::Inline { .. } => "inlined".into(),
        Op::AddDef { intent, .. } => format!("add-def ({})", intent_name(intent)),
        Op::Delete { intent, .. } => format!("deleted ({})", intent_name(intent)),
        Op::EditDef { intent, .. } => format!("edit-def ({})", intent_name(intent)),
        Op::Merge { other } => format!("merged {}", other.short()),
        Op::Undo => "undo".into(),
        Op::New { change } => format!("new change {}", change.short()),
        Op::Describe { msg } => format!("described: {msg}"),
        Op::Branch { name } => format!("branch {name}"),
        Op::Absorb => "absorbed hand edits".into(),
    }
}

pub use svc_repo::text::{class as class_name, intent as intent_name};

pub fn conflict_line(c: &ConflictOut) -> String {
    match &c.conflict {
        Conflict::Binding { name, was, now, at, .. } => format!(
            "binding conflict in {}: `{name}` at {}:{} meant {}, now means {}",
            c.name,
            at.line,
            at.col,
            ident(was),
            ident(now)
        ),
        Conflict::Attr { .. } => format!("attribute conflict on {}", c.name),
        Conflict::Content { .. } => format!("content conflict on {}", c.name),
        Conflict::AddAdd { key, .. } => format!("add/add: {}", key.name),
        Conflict::DeleteEdit { .. } => format!("delete/edit on {}", c.name),
    }
}

pub fn kind_glyph(k: Kind) -> &'static str {
    match k {
        Kind::Fn | Kind::JsFunction | Kind::JsMethod | Kind::JsStaticMethod => "ƒ",
        Kind::Struct | Kind::Enum | Kind::Union | Kind::JsClass => "◇",
        Kind::Trait => "◈",
        Kind::Impl => "⊕",
        Kind::Const | Kind::Static | Kind::JsField | Kind::JsStaticField | Kind::JsDeclarator => "•",
        Kind::Mod => "▸",
        Kind::TypeAlias => "≡",
        Kind::Macro => "!",
        Kind::JsGetter => "get",
        Kind::JsSetter => "set",
        Kind::JsStaticBlock => "{}",
        Kind::Opaque => "·",
    }
}

fn ident(r: &svc_core::IdentRef) -> String {
    match r {
        svc_core::IdentRef::Local(slot, _) => format!("local ${}", slot.0),
        svc_core::IdentRef::Entity(id) if *id == svc_core::EntityId::SELF => "itself".into(),
        svc_core::IdentRef::Entity(id) => format!("⟨{}⟩", id.short()),
        svc_core::IdentRef::Free(n) => format!("free `{n}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_def_reads_text_or_bytes_src() {
        let with_text: ShowDef = serde_json::from_str(
            r#"{"canonical":"fn f()","text":"fn f() {}\n"}"#,
        )
        .unwrap();
        assert_eq!(with_text.source(), "fn f() {}\n");

        let from_src: ShowDef = serde_json::from_str(
            r#"{"canonical":"struct C","bytes":{"src":[115,116,114,117,99,116,32,67,32,123,125]}}"#,
        )
        .unwrap();
        assert_eq!(from_src.source(), "struct C {}");
    }
}
