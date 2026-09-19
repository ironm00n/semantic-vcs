//! Named checkouts sharing one store — jj's `workspace add|list|forget`, not git worktrees'
//! per-branch lock and not concurrent writers. The default checkout is the directory holding
//! `.svc/`; a named one is any directory holding a `.svc-workspace` pointer at that store and
//! its own `root` row ([`crate::store::WorkspaceRow`]). Snapshots, heads, branches and the
//! op log are shared, so an op in one checkout is visible to `svc log` in every other.
//!
//! The redb lock still serialises processes: two checkouts run *sequential*
//! CLI invocations against one file, they do not write simultaneously.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use svc_core::{ChangeId, Error, Result, SnapshotId};

use crate::repo::{IGNORE_FILE, Repo};
use crate::store::WorkspaceRow;

/// File at a named checkout's root pointing at the shared store.
pub const POINTER_FILE: &str = ".svc-workspace";
/// The name `list` reports for the checkout that holds `.svc/` itself.
pub const DEFAULT_WORKSPACE: &str = "default";

/// Contents of `.svc-workspace` (JSON so a human can read and repair it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePointer {
    /// Absolute path of the shared `.svc/store.redb`.
    pub store: PathBuf,
    pub name: String,
}

impl WorkspacePointer {
    /// `Ok(None)` when `dir` has no pointer file.
    pub fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join(POINTER_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(Error::backend)?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Error::Other(format!("{}: {e}", path.display())))
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        std::fs::write(dir.join(POINTER_FILE), text + "\n").map_err(Error::backend)
    }
}

/// One row of `svc workspace list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOut {
    pub name: String,
    pub path: PathBuf,
    /// The change checked out there and its snapshot; `None` only for a row whose checkout
    /// was never rendered (should not happen through `add`).
    pub change: Option<ChangeId>,
    pub snapshot: Option<SnapshotId>,
    /// True for the checkout `repo` was opened from.
    pub current: bool,
    /// The checkout's `root` is no longer its change's head (another checkout amended the
    /// change); mutations there refuse until [`update_stale`].
    pub stale: bool,
}

/// `svc workspace update-stale`: move this checkout to its change's current head and render
/// it — the way out of the state [`Repo::is_stale`] reports. Refuses to discard hand edits.
/// A checkout that is not stale is left alone. Not an op-log entry (nothing in the store
/// changes but this checkout's `root`), exactly like `svc checkout`.
pub fn update_stale(repo: &Repo) -> Result<WorkspaceOut> {
    let store = repo.store();
    let cur = repo.current()?;
    let head = store.head(cur.change)?;
    if head != cur.id() {
        if !repo.working_copy_clean()? {
            return Err(Error::Other(
                "working copy has edits and the checkout is stale; stash them by hand first".into(),
            ));
        }
        let snap = store.get_snapshot(head)?;
        store.set_render_pending(true)?;
        store.set_root(head)?;
        repo.render_to_disk(&snap)?;
        store.set_render_pending(false)?;
    }
    let name = repo.workspace().unwrap_or(DEFAULT_WORKSPACE).to_string();
    Ok(WorkspaceOut {
        name,
        path: repo.root_dir().to_path_buf(),
        change: Some(cur.change),
        snapshot: Some(head),
        current: true,
        stale: false,
    })
}

/// `svc workspace add <name> <path> [--at <change>]`: create `path` (must be empty or absent),
/// point it at `repo`'s store, and render the head of `at` — or `repo`'s current snapshot —
/// there. The new checkout is *not* made current; open it with [`Repo::discover`].
///
/// Deliberately not an op-log entry: `Op` is a frozen `svc-core` shape and a checkout is
/// metadata about where a snapshot is rendered, not a change to any snapshot (jj likewise
/// keeps workspaces out of the op graph's content).
pub fn add(repo: &Repo, name: &str, path: &Path, at: Option<ChangeId>) -> Result<WorkspaceOut> {
    validate_name(name)?;
    let store = repo.store();
    if repo.store.workspace_row(name)?.is_some() {
        return Err(Error::Other(format!("workspace {name:?} already exists")));
    }
    std::fs::create_dir_all(path).map_err(Error::backend)?;
    let path = path.canonicalize().map_err(Error::backend)?;
    if std::fs::read_dir(&path).map_err(Error::backend)?.next().is_some() {
        return Err(Error::Other(format!("{} is not empty", path.display())));
    }
    if repo.store.workspaces()?.iter().any(|(_, r)| r.path == path) {
        return Err(Error::Other(format!("{} is already a workspace", path.display())));
    }
    let snapshot_id = match at {
        Some(change) => store.head(change)?,
        None => store.root()?,
    };
    let snapshot = store.get_snapshot(snapshot_id)?;

    // Pointer first: a crash after this leaves a directory `discover` rejects loudly
    // ("workspace not found") rather than a row pointing at nothing.
    WorkspacePointer {
        store: repo.store_path().to_path_buf(),
        name: name.to_string(),
    }
    .write(&path)?;
    if let Ok(ignore) = std::fs::read(repo.root_dir().join(IGNORE_FILE)) {
        std::fs::write(path.join(IGNORE_FILE), ignore).map_err(Error::backend)?;
    }
    repo.store.set_workspace_row(
        name,
        &WorkspaceRow {
            path: path.clone(),
            root: Some(snapshot_id),
            render_pending: true,
            open_changeset: None,
        },
    )?;
    repo.render_into(&path, &snapshot)?;
    repo.store.set_workspace_row(
        name,
        &WorkspaceRow {
            path: path.clone(),
            root: Some(snapshot_id),
            render_pending: false,
            open_changeset: None,
        },
    )?;
    Ok(WorkspaceOut {
        name: name.to_string(),
        path,
        change: Some(snapshot.change),
        snapshot: Some(snapshot_id),
        current: false,
        stale: false,
    })
}

/// `svc workspace list`: the default checkout first, then named ones by name.
pub fn list(repo: &Repo) -> Result<Vec<WorkspaceOut>> {
    let store = repo.store();
    // (change, stale) for a checkout root: stale when the change's head has moved past it.
    let describe = |root: Option<SnapshotId>| -> Result<(Option<ChangeId>, bool)> {
        Ok(match root {
            Some(id) => {
                let change = store.get_snapshot(id)?.change;
                (Some(change), store.head(change)? != id)
            }
            None => (None, false),
        })
    };
    let default_root = default_root_dir(repo.store_path());
    let default = repo.store.default_root()?;
    let (change, stale) = describe(default)?;
    let mut out = vec![WorkspaceOut {
        name: DEFAULT_WORKSPACE.into(),
        path: default_root,
        change,
        snapshot: default,
        current: repo.workspace().is_none(),
        stale,
    }];
    for (name, row) in repo.store.workspaces()? {
        let (change, stale) = describe(row.root)?;
        out.push(WorkspaceOut {
            current: repo.workspace() == Some(name.as_str()),
            change,
            snapshot: row.root,
            name,
            path: row.path,
            stale,
        });
    }
    Ok(out)
}

/// `svc workspace forget <name>`: drop the row and the pointer file. Files stay on disk;
/// nothing in the store is lost because a checkout owns no snapshots. Refuses the default
/// checkout and the one `repo` was opened from.
pub fn forget(repo: &Repo, name: &str) -> Result<WorkspaceOut> {
    if name == DEFAULT_WORKSPACE || name.is_empty() {
        return Err(Error::Other("the default workspace cannot be forgotten".into()));
    }
    if repo.workspace() == Some(name) {
        return Err(Error::Other(format!(
            "workspace {name:?} is the current one; run this from another checkout"
        )));
    }
    let row = repo
        .store
        .workspace_row(name)?
        .ok_or_else(|| Error::NotFound(format!("workspace {name:?}")))?;
    let change = match row.root {
        Some(id) => Some(repo.store().get_snapshot(id)?.change),
        None => None,
    };
    repo.store.remove_workspace(name)?;
    let pointer = row.path.join(POINTER_FILE);
    if pointer.is_file() {
        std::fs::remove_file(pointer).map_err(Error::backend)?;
    }
    Ok(WorkspaceOut {
        name: name.to_string(),
        path: row.path,
        change,
        snapshot: row.root,
        current: false,
        stale: false,
    })
}

/// The directory that holds `.svc/`, from the store file's path.
pub fn default_root_dir(store_path: &Path) -> PathBuf {
    store_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == DEFAULT_WORKSPACE {
        return Err(Error::Other(format!("{name:?} is reserved for the default workspace")));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Error::Other(format!(
            "workspace name {name:?}: use letters, digits, '-', '_' or '.'"
        )));
    }
    Ok(())
}
