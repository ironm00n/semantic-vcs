//! A checked-out repository: the `.svc/` store plus the working copy around it.
//!
//! Every mutating verb goes through [`Repo::mutate`], which snapshots the working copy
//! before, commits, records the op, and renders after (design §4). Nothing here decides
//! what a snapshot *contains* — that is `svc_core::engine`'s job; this module only moves
//! bytes between disk and store and keeps `root`/`heads`/the op log consistent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use svc_core::engine::{render, snapshot_files as engine_snapshot_files};
use svc_core::{
    CHANGESET_TTL_MS, ChangeId, ChangeSetId, Error, JsLang, Langs, ObservedClass, Op, OpIx,
    OpLogEntry, RelPath, Result, RustLang, Snapshot, SnapshotId, Store, Timestamp, View,
};

use crate::store::RedbStore;
use crate::workspace::{POINTER_FILE, WorkspacePointer};

pub const STORE_DIR: &str = ".svc";
pub const STORE_FILE: &str = "store.redb";
pub const IGNORE_FILE: &str = ".svcignore";

/// Directories never walked, on top of `.svcignore`.
const ALWAYS_IGNORED: &[&str] = &[STORE_DIR, ".git", ".jj", "target", "node_modules"];

/// How long `open` keeps retrying redb's lock (design decision §11) unless `SVC_LOCK_TIMEOUT_MS`
/// or [`Repo::open_with`] says otherwise. One `Repo` is one exclusive store session: redb
/// refuses a second handle on the file, from another process or this one, until the first
/// is dropped. Every verb is therefore serialised and atomic with respect to every other.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
pub const LOCK_TIMEOUT_ENV: &str = "SVC_LOCK_TIMEOUT_MS";

pub fn lock_timeout() -> Duration {
    std::env::var(LOCK_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_LOCK_TIMEOUT)
}

pub struct Repo {
    root: PathBuf,
    store_path: PathBuf,
    pub(crate) store: RedbStore,
    langs: Langs,
}

/// What `mutate` did, for the verb to print.
#[derive(Clone, Debug)]
pub struct Mutation {
    pub ix: OpIx,
    pub entry: OpLogEntry,
    /// The snapshot the op produced (equal to `entry.after.root` unless the op was a no-op).
    pub snapshot: SnapshotId,
    /// Set when a stale `open_changeset` row was closed on the way (design §5.6).
    pub closed_stale_changeset: Option<ChangeSetId>,
}

impl Repo {
    pub fn default_langs() -> Langs {
        Langs::new(vec![Box::new(RustLang), Box::new(JsLang)])
    }

    /// Nearest ancestor of `start` (inclusive) that is a checkout: it holds `.svc/store.redb`
    /// (the default checkout) or a `.svc-workspace` pointer (a named one).
    pub fn find_root(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .find(|p| Self::is_checkout(p))
            .map(Path::to_path_buf)
    }

    fn is_checkout(dir: &Path) -> bool {
        dir.join(STORE_DIR).join(STORE_FILE).is_file() || dir.join(POINTER_FILE).is_file()
    }

    /// Creates `.svc/` and the first snapshot from every tracked file. Refuses to re-init.
    pub fn init(root: &Path, langs: Langs) -> Result<Self> {
        let dir = root.join(STORE_DIR);
        if dir.join(STORE_FILE).exists() {
            return Err(Error::Other(format!("{} already exists", dir.display())));
        }
        if root.join(POINTER_FILE).exists() {
            return Err(Error::Other(format!(
                "{} is already a workspace of another store",
                root.display()
            )));
        }
        std::fs::create_dir_all(&dir).map_err(Error::backend)?;
        let store_path = dir.join(STORE_FILE);
        let store = RedbStore::create(&store_path)?;
        let repo = Self {
            root: root.to_path_buf(),
            store_path,
            store,
            langs,
        };
        let change = ChangeId::new();
        let files = repo.tracked_files()?;
        let snapshot = repo.snapshot_files(&files, None, change)?;
        // The op log needs a "before"; an empty snapshot makes init blame as `Added`.
        let empty = repo.store.put_snapshot(&Snapshot {
            entities: BTreeMap::new(),
            files: BTreeMap::new(),
            ..snapshot.clone()
        })?;
        repo.commit_snapshot(&snapshot)?;
        let view = repo.view()?;
        repo.store.append_op(&OpLogEntry {
            op: Op::New { change },
            observed: None,
            at: now(),
            group: None,
            before: View {
                root: empty,
                heads: BTreeMap::new(),
            },
            after: view,
        })?;
        Ok(repo)
    }

    /// Opens an existing repo, waiting out another process's lock, and finishes any render
    /// a crashed predecessor left pending.
    ///
    /// `root` is either the default checkout (holds `.svc/store.redb`) or a named workspace
    /// (holds a `.svc-workspace` pointer at the shared store).
    pub fn open(root: &Path, langs: Langs) -> Result<Self> {
        Self::open_with(root, langs, lock_timeout())
    }

    /// [`Repo::open`] with an explicit bound on how long to wait for another session.
    pub fn open_with(root: &Path, langs: Langs, wait: Duration) -> Result<Self> {
        let own = root.join(STORE_DIR).join(STORE_FILE);
        let (store_path, workspace) = if own.is_file() {
            (own, None)
        } else if let Some(ptr) = WorkspacePointer::read(root)? {
            (ptr.store, Some(ptr.name))
        } else {
            return Err(Error::Other(format!("no {STORE_DIR} in {}", root.display())));
        };
        if !store_path.is_file() {
            return Err(Error::Other(format!(
                "{} points at a missing store {}",
                root.join(POINTER_FILE).display(),
                store_path.display()
            )));
        }
        let store = Self::open_store(&store_path, wait)?.with_workspace(workspace.as_deref())?;
        let repo = Self {
            root: root.to_path_buf(),
            store_path,
            store,
            langs,
        };
        if repo.store.render_pending()? {
            repo.render_to_disk(&repo.current()?)?;
            repo.store.set_render_pending(false)?;
        }
        Ok(repo)
    }

    /// Opens the redb file, waiting up to `wait` for another session to end.
    fn open_store(path: &Path, wait: Duration) -> Result<RedbStore> {
        let started = Instant::now();
        let mut backoff = Duration::from_millis(10);
        loop {
            match RedbStore::open(path) {
                Ok(s) => return Ok(s),
                Err(e) if RedbStore::is_already_open(&e) => {
                    if started.elapsed() >= wait {
                        return Err(Error::Other(format!(
                            "store busy: another svc session held {} for {}ms (raise {LOCK_TIMEOUT_ENV})",
                            path.display(),
                            wait.as_millis()
                        )));
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// True when this checkout's `root` is no longer its change's head — another checkout
    /// (or `svc checkout` of an older evolution) moved on. Mutating from here would silently
    /// overwrite that head, so [`Repo::mutate`] refuses until `workspace::update_stale`.
    pub fn is_stale(&self) -> Result<bool> {
        let cur = self.current()?;
        Ok(self.store.head(cur.change)? != cur.id())
    }

    fn refuse_if_stale(&self) -> Result<()> {
        if self.is_stale()? {
            let cur = self.current()?;
            return Err(Error::Other(format!(
                "checkout is behind change {}: its head moved to {} (run `svc workspace update-stale`, or `svc checkout` it)",
                cur.change.short(),
                self.store.head(cur.change)?.short()
            )));
        }
        Ok(())
    }

    pub fn discover(start: &Path, langs: Langs) -> Result<Self> {
        let root = Self::find_root(start)
            .ok_or_else(|| Error::Other(format!("no {STORE_DIR} above {}", start.display())))?;
        Self::open(&root, langs)
    }

    pub fn store(&self) -> &dyn Store {
        &self.store
    }

    /// The concrete store, for what the `Store` trait does not cover (workspaces, op attribution).
    pub fn redb(&self) -> &RedbStore {
        &self.store
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    /// The shared `.svc/store.redb` this checkout reads, wherever the checkout itself lives.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// The named workspace this checkout is, or `None` for the default one.
    pub fn workspace(&self) -> Option<&str> {
        self.store.workspace()
    }

    pub fn langs(&self) -> &Langs {
        &self.langs
    }

    pub fn view(&self) -> Result<View> {
        Ok(View {
            root: self.store.root()?,
            heads: self.store.heads()?.into_iter().collect(),
        })
    }

    /// The working-copy snapshot and its id.
    pub fn current(&self) -> Result<Snapshot> {
        self.store.get_snapshot(self.store.root()?)
    }

    pub fn current_change(&self) -> Result<ChangeId> {
        Ok(self.current()?.change)
    }

    /// Branch name first, then unique change-id prefix.
    pub fn resolve_change(&self, s: &str) -> Result<ChangeId> {
        if let Some(id) = self.store.branch(s)? {
            return Ok(id);
        }
        self.store.resolve_prefix(s)
    }

    /// Every file under the root whose extension some `Lang` claims, minus ignores.
    pub fn tracked_files(&self) -> Result<BTreeMap<RelPath, Vec<u8>>> {
        let ignore = self.ignore_patterns()?;
        let mut out = BTreeMap::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).map_err(Error::backend)? {
                let entry = entry.map_err(Error::backend)?;
                let path = entry.path();
                let rel = path
                    .strip_prefix(&self.root)
                    .map_err(|e| Error::Other(e.to_string()))?
                    .to_string_lossy()
                    .into_owned();
                if is_ignored(&rel, &ignore) {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = RelPath::new(rel).map_err(Error::InvalidPath)?;
                if self.langs.for_path(&rel).is_some() {
                    out.insert(rel, std::fs::read(&path).map_err(Error::backend)?);
                }
            }
        }
        Ok(out)
    }

    fn ignore_patterns(&self) -> Result<Vec<String>> {
        let mut pats: Vec<String> = ALWAYS_IGNORED.iter().map(|s| s.to_string()).collect();
        if let Ok(text) = std::fs::read_to_string(self.root.join(IGNORE_FILE)) {
            pats.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| l.trim_end_matches('/').trim_start_matches('/').to_string()),
            );
        }
        Ok(pats)
    }

    /// Bytes-in → snapshot-out. Delegates to `svc_core::engine::snapshot_files` (design note §7).
    fn snapshot_files(
        &self,
        files: &BTreeMap<RelPath, Vec<u8>>,
        prev: Option<&Snapshot>,
        change: ChangeId,
    ) -> Result<Snapshot> {
        engine_snapshot_files(&self.store, &self.langs, files, prev, change)
    }

    /// True when rendering the current snapshot reproduces the tracked files byte for byte.
    pub fn working_copy_clean(&self) -> Result<bool> {
        let rendered = render(&self.current()?, &self.store, &self.langs, false)?;
        Ok(rendered.files == self.tracked_files()?)
    }

    /// Reconcile hand edits into the current change (design §2.5, §4's wrapper): re-snapshot
    /// the tracked files against the current snapshot, amend unless identical, and record an
    /// `Absorb` op whose `after.root` is the new snapshot (so O5 can replay it). Returns the
    /// previous snapshot and the new id when something was absorbed.
    pub fn absorb(&self) -> Result<Option<(Snapshot, SnapshotId)>> {
        if self.working_copy_clean()? {
            return Ok(None);
        }
        self.refuse_if_stale()?;
        let cur = self.current()?;
        let files = self.tracked_files()?;
        let next = self.snapshot_files(&files, Some(&cur), cur.change)?;
        if next.content_eq(&cur) {
            return Ok(None);
        }
        let before = self.view()?;
        let (group, _) = self.open_group()?;
        let id = self.amend(&cur, next)?;
        let after = self.view()?;
        self.store.append_op(&OpLogEntry {
            op: Op::Absorb,
            observed: None,
            at: now(),
            group,
            before,
            after,
        })?;
        Ok(Some((cur, id)))
    }

    /// Writes `snapshot`, points its change's head and `root` at it.
    pub fn commit_snapshot(&self, snapshot: &Snapshot) -> Result<SnapshotId> {
        let id = self.store.put_snapshot(snapshot)?;
        self.store.set_head(snapshot.change, id)?;
        self.store.set_root(id)?;
        Ok(id)
    }

    /// Rewrites the current change: `next` gets `cur` as its predecessor and inherits its
    /// parents. Identical content is a no-op (design §2.5 step 3) and returns `cur`'s id.
    pub fn amend(&self, cur: &Snapshot, mut next: Snapshot) -> Result<SnapshotId> {
        let cur_id = cur.id();
        if next.content_eq(cur) {
            return Ok(cur_id);
        }
        next.parents = cur.parents.clone();
        next.predecessors = vec![cur_id];
        next.change = cur.change;
        self.commit_snapshot(&next)
    }

    /// Snapshot-before / commit / log / render-after, for every mutating verb.
    ///
    /// `f` receives the (absorbed) current snapshot and returns the snapshot the op yields,
    /// already wired (`parents`/`predecessors`/`change`) — use [`Repo::amend`] for rewrites
    /// of the current change or build a fresh one for `new`/`branch`.
    pub fn mutate(
        &self,
        op: Op,
        observed: Option<ObservedClass>,
        f: impl FnOnce(&Repo, &Snapshot) -> Result<SnapshotId>,
    ) -> Result<Mutation> {
        self.absorb()?;
        self.refuse_if_stale()?;
        let before = self.view()?;
        let (group, closed_stale_changeset) = self.open_group()?;
        let cur = self.current()?;
        // From here to `append_op`, head/root/render-pending writes are staged and land in
        // the op's own transaction: a crash never leaves the store a snapshot ahead of
        // the log, and a failing verb publishes nothing (design note §15b).
        self.store.stage();
        let staged = (|| -> Result<(SnapshotId, OpLogEntry)> {
            self.store.set_render_pending(true)?;
            let snapshot = f(self, &cur)?;
            let after = self.view()?;
            Ok((snapshot, OpLogEntry { op, observed, at: now(), group, before, after }))
        })();
        let (snapshot, entry) = match staged {
            Ok(v) => v,
            Err(e) => {
                self.store.discard_staged();
                return Err(e);
            }
        };
        let ix = self.store.append_op(&entry)?;
        self.render_to_disk(&self.current()?)?;
        self.store.set_render_pending(false)?;
        Ok(Mutation {
            ix,
            entry,
            snapshot,
            closed_stale_changeset,
        })
    }

    /// Moves `root` and every head to `view`, records it as `op`, and re-renders.
    /// Heads absent from `view` cannot be removed through the `Store` API and stay behind.
    pub fn restore_view(&self, view: &View, op: Op) -> Result<Mutation> {
        self.absorb()?;
        let before = self.view()?;
        let (group, closed_stale_changeset) = self.open_group()?;
        self.store.stage();
        let staged = (|| -> Result<OpLogEntry> {
            self.store.set_render_pending(true)?;
            for (change, snap) in &view.heads {
                self.store.set_head(*change, *snap)?;
            }
            self.store.set_root(view.root)?;
            let after = self.view()?;
            Ok(OpLogEntry { op, observed: None, at: now(), group, before, after })
        })();
        let entry = match staged {
            Ok(e) => e,
            Err(e) => {
                self.store.discard_staged();
                return Err(e);
            }
        };
        let ix = self.store.append_op(&entry)?;
        self.render_to_disk(&self.current()?)?;
        self.store.set_render_pending(false)?;
        Ok(Mutation {
            ix,
            entry,
            snapshot: view.root,
            closed_stale_changeset,
        })
    }

    /// The open changeset to stamp on an op, closing a row whose owner is gone (design §5.6).
    pub fn open_group(&self) -> Result<(Option<ChangeSetId>, Option<ChangeSetId>)> {
        let Some(row) = self.store.open_changeset()? else {
            return Ok((None, None));
        };
        let alive = match row.pid {
            Some(pid) => Path::new("/proc").join(pid.to_string()).exists(),
            None => now().saturating_sub(row.opened_at) < CHANGESET_TTL_MS,
        };
        if alive {
            Ok((Some(row.id), None))
        } else {
            self.store.set_open_changeset(None)?;
            Ok((None, Some(row.id)))
        }
    }

    /// Writes every file of `snapshot` via temp + `rename(2)`, then deletes tracked files it
    /// no longer contains (design §5.5). Never touches untracked files.
    pub fn render_to_disk(&self, snapshot: &Snapshot) -> Result<()> {
        let rendered = self.render_into(&self.root, snapshot)?;
        for rel in self.tracked_files()?.keys() {
            if !rendered.contains_key(rel) {
                std::fs::remove_file(self.root.join(rel.as_str())).map_err(Error::backend)?;
            }
        }
        Ok(())
    }

    /// The write half of [`Repo::render_to_disk`] aimed at any directory: every file of
    /// `snapshot` lands under `dir` (temp + `rename(2)`), nothing is deleted. Used to seed a
    /// new workspace. Returns what was rendered.
    pub(crate) fn render_into(
        &self,
        dir: &Path,
        snapshot: &Snapshot,
    ) -> Result<BTreeMap<RelPath, Vec<u8>>> {
        let rendered = render(snapshot, &self.store, &self.langs, false)?;
        for (rel, bytes) in &rendered.files {
            let path = dir.join(rel.as_str());
            if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(Error::backend)?;
            }
            let tmp = path.with_extension(format!(
                "{}.svc-tmp",
                path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ));
            std::fs::write(&tmp, bytes).map_err(Error::backend)?;
            std::fs::rename(&tmp, &path).map_err(Error::backend)?;
        }
        Ok(rendered.files)
    }
}

fn is_ignored(rel: &str, patterns: &[String]) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    patterns.iter().any(|p| {
        if p.contains('/') {
            rel == p || rel.starts_with(&format!("{p}/"))
        } else {
            name == p
        }
    })
}

pub fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
