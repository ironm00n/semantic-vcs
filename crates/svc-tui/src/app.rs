use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::Value;
use svc_agent::{AgentCommand, AgentEvent, PermissionAsk};
use svc_core::{Conflict, Op};
use svc_repo::{BlameEntry, ChangeOut, ConflictOut, EntityTouch, OpOut, Touch};
use tokio::sync::mpsc::UnboundedSender;

use crate::data::{Definition, Svc, class_name, conflict_line, describe_op, intent_name, kind_glyph};
use crate::syntax::source_lines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Revisions,
    Entities,
    Oplog,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Browse,
    Queue,
}

/// Every `edit_def` queues (green when declared and observed agree, red when they
/// don't), binding conflicts queue, nothing else. The permission ask and the verdict are one
/// item in two states.
pub enum QueueItem {
    Ask {
        ask: Option<PermissionAsk>,
        tool: String,
        entity: String,
        intent: String,
        definition: String,
        answered: Option<bool>,
    },
    Edit {
        op: OpOut,
        entity: String,
    },
    Binding {
        line: String,
    },
}

impl QueueItem {
    fn pending(&self) -> bool {
        matches!(self, QueueItem::Ask { ask: Some(_), .. })
    }
}

pub struct AgentLink {
    pub commands: UnboundedSender<AgentCommand>,
    pub task: String,
    /// Prepend the entity list to the first prompt (removes the discovery
    /// round trip when the live run is long). `SVC_AGENT_PRESEED=1`.
    pub preseed: bool,
    pub running: bool,
    pub tool_titles: HashMap<String, (String, Value)>,
}

pub struct App {
    pub svc: Svc,
    pub mode: ViewMode,
    pub changes: Vec<ChangeOut>,
    pub revision_state: ListState,
    pub defs: Vec<Definition>,
    pub rows: Vec<(usize, usize)>,
    pub tree_state: ListState,
    pub op_state: ListState,
    pub events: Vec<Line<'static>>,
    pub events_for: Option<String>,
    pub queue: Vec<QueueItem>,
    pub queue_state: ListState,
    pub expanded: HashSet<usize>,
    pub focus: Pane,
    pub agent: Option<AgentLink>,
    pub touched: HashSet<String>,
    pub status: String,
    pub error: Option<String>,
    pub dirty: bool,
    pub need_draw: bool,
    /// When `events_for` is None, wait until this instant before shelling out
    /// to `show-def`/`blame` so holding j/k does not spawn a process per key.
    detail_at: Instant,
    /// The store file's mtime as of the last refresh, probed once a second: a change
    /// is another process (or the agent) publishing, and the panes follow it.
    store_seen: Option<SystemTime>,
    store_probe_at: Instant,
    retry_at: Instant,
    /// Last `Event::Resize` we drew for — `script(1)` and some ptys repeat the
    /// same size, which used to mark every frame dirty and starve input.
    last_size: Option<(u16, u16)>,
    /// Full `svc op log`, newest first, for the change-log strip.
    ops: Vec<OpOut>,
    /// Jump once to an entity that actually has history, not the first `use`.
    picked_story: bool,
    pub should_quit: bool,
    pub log: Vec<String>,
    /// `/` filter over the tree: a substring of the name or file, case-insensitive. While
    /// `typing`, keys go to it; Enter keeps it, Esc clears it. Non-empty = a flat match list.
    pub filter: String,
    /// Every op's root after it, by op index, so an edit-def's "before" is the previous
    /// op's root; and the before→after diffs already computed, by op index.
    roots: std::collections::BTreeMap<u64, String>,
    edit_diffs: HashMap<u64, Vec<Line<'static>>>,
    pub typing: bool,
}

impl App {
    pub fn new(svc: Svc) -> Self {
        let mut app = Self {
            svc,
            mode: ViewMode::Revisions,
            changes: Vec::new(),
            revision_state: ListState::default(),
            defs: Vec::new(),
            rows: Vec::new(),
            tree_state: ListState::default(),
            op_state: ListState::default(),
            events: Vec::new(),
            events_for: None,
            queue: Vec::new(),
            queue_state: ListState::default(),
            expanded: HashSet::new(),
            focus: Pane::Browse,
            agent: None,
            touched: HashSet::new(),
            status: String::new(),
            error: None,
            dirty: true,
            need_draw: true,
            detail_at: Instant::now(),
            store_seen: None,
            store_probe_at: Instant::now() + Duration::from_secs(1),
            retry_at: Instant::now(),
            last_size: None,
            ops: Vec::new(),
            picked_story: false,
            should_quit: false,
            log: Vec::new(),
            filter: String::new(),
            roots: Default::default(),
            edit_diffs: HashMap::new(),
            typing: false,
        };
        app.revision_state.select(Some(0));
        app.tree_state.select(Some(0));
        app.op_state.select(Some(0));
        app
    }

    /// Re-read everything from `svc --json`. Pending asks survive; verdict rows replace answered asks.
    pub fn refresh(&mut self) {
        self.dirty = false;
        let selected_change = self.selected_change().map(|change| change.change);
        match self.svc.heads() {
            // Another `svc` holds this checkout (a long rename in a second terminal): not an
            // error to show, just try again shortly.
            Err(e) if e.contains("checkout busy") => {
                self.dirty = true;
                self.retry_at = Instant::now() + Duration::from_millis(300);
                return;
            }
            Ok(changes) => {
                self.changes = changes;
                let selected = selected_change
                    .and_then(|id| self.changes.iter().position(|change| change.change == id))
                    .or_else(|| self.changes.iter().position(|change| change.current))
                    .or((!self.changes.is_empty()).then_some(0));
                self.revision_state.select(selected);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        match self.svc.list_defs() {
            Err(e) if e.contains("checkout busy") => {
                self.dirty = true;
                self.retry_at = Instant::now() + Duration::from_millis(300);
                return;
            }
            Ok(defs) => {
                self.defs = defs;
                self.rows = self.filtered_rows();
                let n = self.rows.len();
                if n == 0 {
                    self.tree_state.select(None);
                } else if self.tree_state.selected().is_none_or(|s| s >= n) {
                    self.tree_state.select(Some(n.saturating_sub(1)));
                }
            }
            Err(e) => self.error = Some(e),
        }
        let log = self.svc.log().unwrap_or_default();
        self.ops = self.svc.op_log().unwrap_or_else(|_| log.clone());
        if self.ops.is_empty() {
            self.op_state.select(None);
        } else if self.op_state.selected().is_none_or(|index| index >= self.ops.len()) {
            self.op_state.select(Some(0));
        }
        let conflicts = self.svc.conflicts().unwrap_or_default();
        self.roots = log.iter().map(|o| (o.ix.0, o.root_after.to_string())).collect();
        self.edit_diffs.clear();
        self.rebuild_queue(&log, &conflicts);
        self.select_touched_if_needed();
        if self.touched.is_empty() {
            self.pick_story_entity();
        }
        // Lists first; `show-def`/`blame` wait for pump so the first paint is not a hang.
        self.invalidate_detail();
        self.detail_at = Instant::now();
        self.store_seen = self.svc.store_changed_at();
        self.store_probe_at = Instant::now() + Duration::from_secs(1);
        self.need_draw = true;
    }

    /// Once a second: did anything publish to the store since the last refresh?
    fn store_moved(&mut self) -> bool {
        if Instant::now() < self.store_probe_at {
            return false;
        }
        self.store_probe_at = Instant::now() + Duration::from_secs(1);
        self.svc.store_changed_at() != self.store_seen
    }

    /// Store refresh (if dirty or another process published) and the deferred right-pane load.
    pub fn pump(&mut self) {
        if self.store_moved() {
            self.dirty = true;
            if self.agent.as_ref().is_none_or(|a| !a.running) {
                self.status = "the store changed under another process; re-read".into();
            }
        }
        if self.dirty && Instant::now() >= self.retry_at {
            self.refresh();
        }
        if self.events_for.is_none() && Instant::now() >= self.detail_at {
            self.load_events();
            self.need_draw = true;
        }
    }

    /// Newest journal subject that is still in the tree, else the first real item.
    fn pick_story_entity(&mut self) {
        if self.picked_story {
            return;
        }
        self.picked_story = true;
        for op in &self.ops {
            if matches!(op.op, Op::New { .. } | Op::Branch { .. }) {
                continue;
            }
            let Some(name) = op.subject.as_deref() else {
                continue;
            };
            if let Some(idx) = self.rows.iter().position(|(i, _)| self.defs[*i].name == name) {
                self.tree_state.select(Some(idx));
                return;
            }
        }
        if let Some(idx) = self.rows.iter().position(|(i, _)| !is_synth(&self.defs[*i])) {
            self.tree_state.select(Some(idx));
        }
    }

    fn invalidate_detail(&mut self) {
        self.events_for = None;
        self.events = vec![Line::from("…").dark_gray()];
        self.detail_at = Instant::now() + Duration::from_millis(40);
        self.need_draw = true;
    }

    /// After an agent op, jump the tree to a touched entity so the right pane is not
    /// stuck on the first `use` still showing only the init event.
    fn select_touched_if_needed(&mut self) {
        if self.touched.is_empty() {
            return;
        }
        let current_touched = self
            .selected_def()
            .is_some_and(|d| self.touched.contains(&d.name) || self.touched.contains(&d.id));
        if current_touched {
            return;
        }
        if let Some(idx) = self.rows.iter().position(|(i, _)| {
            let d = &self.defs[*i];
            self.touched.contains(&d.name) || self.touched.contains(&d.id)
        }) {
            self.tree_state.select(Some(idx));
        }
    }

    fn rebuild_queue(&mut self, log: &[OpOut], conflicts: &[ConflictOut]) {
        let mut keep: Vec<QueueItem> = Vec::new();
        let old = std::mem::take(&mut self.queue);
        let mut answered_entities: Vec<String> = Vec::new();
        for item in old {
            if let QueueItem::Ask { ask, answered, entity, .. } = &item {
                if ask.is_some() {
                    keep.push(item);
                    continue;
                }
                if answered.is_some() {
                    answered_entities.push(entity.clone());
                    keep.push(item);
                }
            }
        }
        let mut verdicts: Vec<QueueItem> = Vec::new();
        for op in log.iter().rev() {
            if let Op::EditDef { id, .. } = &op.op {
                let entity = op.subject.clone().unwrap_or_else(|| self.name_of(&id.to_string()));
                verdicts.push(QueueItem::Edit {
                    op: op.clone(),
                    entity,
                });
            }
        }
        // One item in two states: an answered ask whose verdict has arrived collapses into it.
        for v in &verdicts {
            if let QueueItem::Edit { entity, .. } = v {
                if let Some(pos) = keep.iter().position(
                    |k| matches!(k, QueueItem::Ask { ask: None, entity: e, .. } if e == entity),
                ) {
                    keep.remove(pos);
                }
            }
        }
        let mut queue = keep;
        queue.extend(verdicts.into_iter().rev());
        for c in conflicts {
            if matches!(c.conflict, Conflict::Binding { .. }) {
                queue.push(QueueItem::Binding {
                    line: conflict_line(c),
                });
            }
        }
        self.queue = queue;
        let n = self.queue.len();
        if n == 0 {
            self.queue_state.select(None);
        } else if self.queue_state.selected().is_none_or(|s| s >= n) {
            self.queue_state.select(Some(0));
        }
    }

    /// The entity list as the agent's `list_defs` would report it, for pre-seeding.
    fn preseed_text(&self) -> Option<String> {
        if self.defs.is_empty() {
            return None;
        }
        let mut s = String::from("Definitions in this repository (svc list-defs; refer to them by name or id):\n");
        for d in &self.defs {
            s.push_str(&format!("- {:?} {} in {} (id {})\n", d.kind, d.name, d.file, d.id));
        }
        Some(s)
    }

    pub fn name_of(&self, id: &str) -> String {
        self.defs
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| id.chars().take(8).collect())
    }

    fn selected_change(&self) -> Option<&ChangeOut> {
        self.revision_state
            .selected()
            .and_then(|index| self.changes.get(index))
    }

    fn selected_def(&self) -> Option<&Definition> {
        self.tree_state
            .selected()
            .and_then(|s| self.rows.get(s))
            .map(|(i, _)| &self.defs[*i])
    }

    fn selected_op(&self) -> Option<&OpOut> {
        self.op_state.selected().and_then(|index| self.ops.get(index))
    }

    pub fn load_events(&mut self) {
        match self.mode {
            ViewMode::Revisions => self.load_change_events(),
            ViewMode::Entities => self.load_entity_events(),
            ViewMode::Oplog => self.load_op_events(),
        }
    }

    fn load_change_events(&mut self) {
        let Some(change) = self.selected_change().cloned() else {
            self.events = vec![Line::from("no revisions")];
            self.events_for = Some("change:".into());
            return;
        };
        let key = format!("change:{}", change.change);
        if self.events_for.as_deref() == Some(&key) {
            return;
        }
        self.events_for = Some(key);
        let mut lines = vec![
            Line::from(change.message.clone()).bold(),
            Line::from(format!("change {}  snapshot {}", change.short, change.snapshot.short())).dark_gray(),
        ];
        if !change.branches.is_empty() {
            lines.push(Line::from(format!("branches  {}", change.branches.join(", "))).cyan());
        }
        lines.push(Line::from(""));
        match self.svc.evolog(&change.change.to_string()) {
            Ok(entries) if entries.is_empty() => lines.push(Line::from("no rewrite history").dark_gray()),
            Ok(entries) => {
                for entry in entries {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{} ", entry.snapshot.short()), Style::default().fg(Color::DarkGray)),
                        Span::raw(if entry.message.is_empty() { "(no description)".into() } else { entry.message }),
                    ]));
                    if entry.deltas.is_empty() {
                        lines.push(Line::from(format!("  {} entities", entry.entities)).dark_gray());
                    } else {
                        lines.extend(entry.deltas.iter().map(entity_touch_line));
                    }
                }
            }
            Err(e) => lines.push(Line::from(format!("evolog failed: {e}")).red()),
        }
        self.events = lines;
        self.store_seen = self.svc.store_changed_at();
    }

    fn load_entity_events(&mut self) {
        let Some(def) = self.selected_def().cloned() else {
            self.events.clear();
            self.events_for = Some("entity:".into());
            return;
        };
        let key = format!("entity:{}", def.id);
        if self.events_for.as_deref() == Some(&key) {
            return;
        }
        self.events_for = Some(key);
        let mut lines: Vec<Line<'static>> = Vec::new();
        match self.svc.show_def(&def.id) {
            Ok(shown) => {
                let src = shown.source();
                if src.trim().is_empty() {
                    lines.push(Line::from("(empty definition)").dark_gray());
                } else {
                    lines.extend(source_lines(&src, &def.file));
                }
                let canon = shown.canonical.trim();
                if !canon.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!("canonical  {canon}")).dark_gray());
                }
            }
            Err(e) => lines.push(Line::from(format!("show-def failed: {e}")).red()),
        }
        lines.push(Line::from(""));
        match self.svc.blame(&def.id) {
            Ok(entries) if entries.is_empty() => lines.push(Line::from("no events yet").dark_gray()),
            Ok(entries) => {
                lines.push(Line::from("history").dark_gray());
                lines.extend(entries.iter().map(blame_line));
            }
            Err(e) => lines.push(Line::from(format!("blame failed: {e}")).red()),
        }
        self.events = lines;
        // show-def/blame open the store and bump its mtime; if we keep the
        // pre-spawn stamp, the 1 s probe thinks another process published.
        self.store_seen = self.svc.store_changed_at();
    }

    fn load_op_events(&mut self) {
        let Some(op) = self.selected_op().cloned() else {
            self.events = vec![Line::from("no operations")];
            self.events_for = Some("op:".into());
            return;
        };
        let key = format!("op:{}", op.ix.0);
        if self.events_for.as_deref() == Some(&key) {
            return;
        }
        self.events_for = Some(key);
        let group = op
            .group
            .map(|id| id.to_string().chars().take(8).collect::<String>())
            .unwrap_or_else(|| "ungrouped".into());
        self.events = vec![
            op_log_line(&op),
            Line::from(format!("group {group}  root {}  at {}", op.root_after.short(), op.at)).dark_gray(),
        ];
    }

    pub fn handle_key(&mut self, event: &Event) {
        if let Event::Resize(w, h) = *event {
            if self.last_size == Some((w, h)) {
                return;
            }
            self.last_size = Some((w, h));
            self.need_draw = true;
            return;
        }
        let Event::Key(key) = event else { return };
        let repeatable = matches!(
            key.code,
            KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Down | KeyCode::Up
        );
        if key.kind != KeyEventKind::Press && !(key.kind == KeyEventKind::Repeat && repeatable) {
            return;
        }
        self.need_draw = true;
        if self.filter_key(key.code) {
            return;
        }
        match key.code {
            KeyCode::Esc if self.mode == ViewMode::Entities && !self.filter.is_empty() => {
                self.filter.clear();
                self.apply_filter();
            }
            KeyCode::Char('/') if self.mode == ViewMode::Entities => self.typing = true,
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Char('h') => self.set_mode(ViewMode::Revisions),
            KeyCode::Char('e') => self.set_mode(if self.mode == ViewMode::Entities {
                ViewMode::Revisions
            } else {
                ViewMode::Entities
            }),
            KeyCode::Char('o') => self.set_mode(if self.mode == ViewMode::Oplog {
                ViewMode::Revisions
            } else {
                ViewMode::Oplog
            }),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Pane::Browse => Pane::Queue,
                    Pane::Queue => Pane::Browse,
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Enter => {
                if self.focus == Pane::Queue {
                    if let Some(i) = self.queue_state.selected() {
                        if !self.expanded.remove(&i) {
                            self.expanded.insert(i);
                            self.load_edit_diff(i);
                        }
                    }
                } else {
                    self.focus = Pane::Queue;
                }
            }
            KeyCode::Char('a') => self.answer(true),
            KeyCode::Char('r') => self.answer(false),
            KeyCode::Char('u') => match self.svc.undo() {
                Ok(_) => {
                    self.status = "undone".into();
                    self.dirty = true;
                }
                Err(e) => self.error = Some(e),
            },
            KeyCode::Char('p') => {
                if let Some(agent) = &mut self.agent {
                    if agent.running {
                        self.status = "agent is still running".into();
                    } else {
                        agent.running = true;
                        let follow_up = format!("Continue: the task is not finished yet. Task: {}", agent.task);
                        let _ = agent.commands.send(AgentCommand::Prompt(follow_up));
                        self.status = "asked the agent to continue".into();
                    }
                }
            }
            KeyCode::Char('c') => {
                if let Some(agent) = &self.agent {
                    for item in &mut self.queue {
                        if let QueueItem::Ask { ask, .. } = item {
                            if let Some(a) = ask.take() {
                                let _ = a.cancel();
                            }
                        }
                    }
                    let _ = agent.commands.send(AgentCommand::Cancel);
                    self.status = "cancel sent".into();
                }
            }
            KeyCode::Char('R') => self.dirty = true,
            _ => {}
        }
    }

    fn filtered_rows(&self) -> Vec<(usize, usize)> {
        if self.filter.is_empty() {
            return tree_rows(&self.defs);
        }
        let needle = self.filter.to_lowercase();
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.name.to_lowercase().contains(&needle) || d.file.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| (i, 0))
            .collect()
    }

    fn apply_filter(&mut self) {
        self.rows = self.filtered_rows();
        self.tree_state.select((!self.rows.is_empty()).then_some(0));
        self.focus = Pane::Browse;
        self.invalidate_detail();
    }

    /// Keys while the filter is being typed. Returns false when the key was not for it.
    fn filter_key(&mut self, code: KeyCode) -> bool {
        if !self.typing {
            return false;
        }
        match code {
            KeyCode::Esc => {
                self.typing = false;
                self.filter.clear();
                self.apply_filter();
            }
            KeyCode::Enter => self.typing = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.apply_filter();
            }
            _ => {}
        }
        true
    }

    /// The expanded rows under a queue item: an ask shows the head of its definition, an
    /// edit-def its before→after diff once loaded, a binding conflict what to do.
    fn queue_detail(&self, q: &QueueItem) -> Vec<Line<'static>> {
        match q {
            QueueItem::Ask { definition, .. } => definition
                .lines()
                .take(5)
                .map(|l| Line::from(format!("      {l}")).dark_gray())
                .collect(),
            QueueItem::Edit { op, .. } => {
                let mut v = vec![Line::from(format!("      op #{}  at {}", op.ix.0, op.at)).dark_gray()];
                match (self.edit_diffs.get(&op.ix.0), &op.op) {
                    (Some(diff), _) => v.extend(diff.iter().cloned()),
                    (None, Op::EditDef { definition, .. }) => {
                        v.extend(definition.lines().take(5).map(|l| Line::from(format!("      {l}")).dark_gray()))
                    }
                    _ => {}
                }
                v
            }
            QueueItem::Binding { .. } => vec![Line::from("      fix the code, or `svc resolve <n> --accept`").dark_gray()],
        }
    }

    /// Before→after of the edit-def at queue row `i`, two `show-def --at` calls, kept until
    /// the next refresh. An older `svc` without `--at` leaves the definition-head fallback.
    fn load_edit_diff(&mut self, i: usize) {
        let Some(QueueItem::Edit { op, .. }) = self.queue.get(i) else { return };
        let Op::EditDef { id, .. } = &op.op else { return };
        let ix = op.ix.0;
        if self.edit_diffs.contains_key(&ix) {
            return;
        }
        let id = id.to_string();
        let after_root = op.root_after.to_string();
        let before_root = self.roots.range(..ix).next_back().map(|(_, r)| r.clone());
        let after = match self.svc.show_def_at(&id, &after_root) {
            Ok(d) => d.source(),
            Err(_) => return,
        };
        let before = before_root
            .and_then(|r| self.svc.show_def_at(&id, &r).ok())
            .map(|d| d.source())
            .unwrap_or_default();
        self.edit_diffs.insert(ix, edit_diff_lines(&before, &after));
    }

    fn set_mode(&mut self, mode: ViewMode) {
        self.mode = mode;
        self.focus = Pane::Browse;
        self.typing = false;
        self.invalidate_detail();
        self.status = match mode {
            ViewMode::Revisions => "revision view".into(),
            ViewMode::Entities => "entity view — h/Esc returns to revisions".into(),
            ViewMode::Oplog => "operation log — h/Esc returns to revisions".into(),
        };
    }

    fn move_sel(&mut self, delta: i32) {
        let (state, len) = if self.focus == Pane::Queue {
            (&mut self.queue_state, self.queue.len())
        } else {
            match self.mode {
                ViewMode::Revisions => (&mut self.revision_state, self.changes.len()),
                ViewMode::Entities => (&mut self.tree_state, self.rows.len()),
                ViewMode::Oplog => (&mut self.op_state, self.ops.len()),
            }
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
        state.select(Some(next));
        if self.focus == Pane::Browse {
            self.invalidate_detail();
        }
    }

    /// Answer the selected pending ask, or the first pending one.
    fn answer(&mut self, allow: bool) {
        let idx = self
            .queue_state
            .selected()
            .filter(|i| self.queue.get(*i).is_some_and(|q| q.pending()))
            .or_else(|| self.queue.iter().position(|q| q.pending()));
        let Some(idx) = idx else {
            self.status = "nothing to answer".into();
            return;
        };
        if let QueueItem::Ask { ask, answered, entity, .. } = &mut self.queue[idx] {
            if let Some(a) = ask.take() {
                let r = if allow { a.allow() } else { a.reject() };
                *answered = Some(allow);
                self.status = match r {
                    Ok(()) => format!("{} edit-def {entity}", if allow { "allowed" } else { "rejected" }),
                    Err(e) => format!("answer failed: {e}"),
                };
            }
        }
    }

    pub fn on_agent_event(&mut self, ev: AgentEvent) {
        self.need_draw = true;
        match ev {
            AgentEvent::Ready { session_id } => {
                self.status = format!("agent session {session_id}");
                let seed = self.preseed_text();
                if let Some(agent) = &mut self.agent {
                    agent.running = true;
                    let prompt = match seed {
                        Some(defs) if agent.preseed => format!("{}\n\n{defs}", agent.task),
                        _ => agent.task.clone(),
                    };
                    let _ = agent.commands.send(AgentCommand::Prompt(prompt));
                }
            }
            AgentEvent::Message { text, .. } => {
                let one_line = text.replace('\n', " ");
                self.status = format!("agent: {one_line}");
                self.log.push(format!("agent: {text}"));
            }
            AgentEvent::Thought { text } => self.log.push(format!("thought: {text}")),
            AgentEvent::ToolCall { id, title, raw_input, .. } => {
                if let Some(entity) = raw_input.as_ref().and_then(|v| v.get("entity")).and_then(Value::as_str) {
                    self.touched.insert(entity.to_string());
                }
                self.status = format!("agent → {title}");
                self.log.push(format!("tool_call {title} {}", raw_input.as_ref().map(Value::to_string).unwrap_or_default()));
                if let Some(agent) = &mut self.agent {
                    agent
                        .tool_titles
                        .insert(id, (title, raw_input.unwrap_or(Value::Null)));
                }
            }
            AgentEvent::ToolCallUpdate { id, status, .. } => {
                if status.as_deref() == Some("completed") || status.as_deref() == Some("failed") {
                    self.dirty = true;
                }
                self.log.push(format!("tool_call_update {id} {}", status.unwrap_or_default()));
            }
            AgentEvent::Permission(ask) => {
                let (tool, input) = self
                    .agent
                    .as_ref()
                    .and_then(|a| a.tool_titles.get(&ask.tool_call_id).cloned())
                    .unwrap_or_else(|| ("?".into(), Value::Null));
                let field = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                self.queue.insert(
                    0,
                    QueueItem::Ask {
                        tool,
                        entity: field("entity"),
                        intent: field("intent"),
                        definition: field("definition"),
                        ask: Some(ask),
                        answered: None,
                    },
                );
                self.queue_state.select(Some(0));
                self.focus = Pane::Queue;
                self.status = "permission requested — a: allow  r: reject".into();
            }
            AgentEvent::Stopped { reason } => {
                // The session stays up: a model that ends its turn early ("Next, I will…")
                // is told to continue with `p`; `q` ends it.
                self.status = format!("agent finished: {reason} — p: continue  q: quit");
                if let Some(agent) = &mut self.agent {
                    agent.running = false;
                }
                self.dirty = true;
            }
            AgentEvent::Log { line } => self.log.push(format!("dsh: {line}")),
            AgentEvent::Closed { error } => {
                if let Some(e) = error {
                    self.error = Some(format!("agent: {e}"));
                }
                if let Some(agent) = &mut self.agent {
                    agent.running = false;
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let pending = self.queue.iter().filter(|q| q.pending()).count();
        let title = format!(
            " svc — {} — {:?} — queue {} ({} pending) ",
            self.svc.root.display(),
            self.mode,
            self.queue.len(),
            pending
        );
        let [title_area, body, queue_area, status_area] = frame.area().layout(&Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(1),
        ]));
        frame.render_widget(Line::from(title).bold().centered(), title_area);
        let [left, right] = body.layout(&Layout::horizontal([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ]));
        self.render_browser(frame, left);
        self.render_preview(frame, right);
        self.render_queue(frame, queue_area);
        self.render_status(frame, status_area);
    }

    fn border(&self, pane: Pane, title: String) -> Block<'static> {
        let style = if self.focus == pane {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        Block::default().borders(Borders::ALL).title(title).border_style(style)
    }

    fn tree_title(&self) -> String {
        if self.filter.is_empty() {
            format!(" entities ({}) ", self.defs.len())
        } else {
            format!(" entities ({}) — /{} ({} match{}) ", self.defs.len(), self.filter, self.rows.len(), if self.rows.len() == 1 { "" } else { "es" })
        }
    }

    fn render_browser(&mut self, frame: &mut Frame, area: Rect) {
        match self.mode {
            ViewMode::Revisions => self.render_revisions(frame, area),
            ViewMode::Entities => self.render_tree(frame, area),
            ViewMode::Oplog => self.render_oplog(frame, area),
        }
    }

    fn render_revisions(&mut self, frame: &mut Frame, area: Rect) {
        let items = self
            .changes
            .iter()
            .map(|change| {
                let marker = if change.current { "@" } else { "○" };
                let branches = if change.branches.is_empty() {
                    String::new()
                } else {
                    format!(" {}", change.branches.join(","))
                };
                let message = if change.message.is_empty() {
                    "(no description)"
                } else {
                    &change.message
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{marker} {}", change.short), Style::default().fg(Color::Cyan)),
                    Span::styled(branches, Style::default().fg(Color::Yellow)),
                    Span::raw(format!("  {message}")),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(self.border(Pane::Browse, format!(" revisions ({}) ", self.changes.len())))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.revision_state);
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|(i, depth)| {
                let d = &self.defs[*i];
                let mark = if self.touched.contains(&d.name) { " ✎" } else { "" };
                let name = if d.name.is_empty() { "(use)".to_string() } else { d.name.clone() };
                ListItem::new(Line::from(vec![
                    Span::raw("  ".repeat(*depth)),
                    Span::styled(format!("{} ", kind_glyph(d.kind)), Style::default().fg(Color::DarkGray)),
                    Span::raw(name),
                    Span::styled(mark, Style::default().fg(Color::Yellow)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(self.border(Pane::Browse, self.tree_title()))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.tree_state);
    }

    fn render_oplog(&mut self, frame: &mut Frame, area: Rect) {
        let items = self.ops.iter().map(op_log_line).map(ListItem::new).collect::<Vec<_>>();
        let list = List::new(items)
            .block(self.border(Pane::Browse, format!(" operation log ({}) ", self.ops.len())))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.op_state);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let mut lines = self.events.clone();
        if lines.is_empty() {
            lines.push(Line::from("…").dark_gray());
        }
        if let Some(agent) = &self.agent {
            lines.push(Line::from(""));
            lines.push(Line::from(format!("agent{}: {}", if agent.running { " (running)" } else { "" }, agent.task)).bold());
            for line in self.log.iter().rev().take(8).rev() {
                lines.push(Line::from(line.chars().take(area.width as usize).collect::<String>()).dark_gray());
            }
        }
        let title = match self.mode {
            ViewMode::Revisions => self
                .selected_change()
                .map(|change| format!(" change {} ", change.short))
                .unwrap_or_else(|| " change details ".into()),
            ViewMode::Entities => self
                .selected_def()
                .map(|def| format!(" {} — {} ", def.name, def.file))
                .unwrap_or_else(|| " entity details ".into()),
            ViewMode::Oplog => self
                .selected_op()
                .map(|op| format!(" operation #{} ", op.ix.0))
                .unwrap_or_else(|| " operation details ".into()),
        };
        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, area);
    }

    fn render_queue(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .queue
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let mut lines = vec![queue_line(q)];
                if self.expanded.contains(&i) {
                    lines.extend(self.queue_detail(q));
                }
                ListItem::new(lines)
            })
            .collect();
        let list = List::new(items)
            .block(self.border(Pane::Queue, " review queue — edit-defs and binding conflicts ".into()))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.queue_state);
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let keys = "j/k move  e entities  / find  o oplog  h/Esc revisions  tab queue  a/r allow/reject  u undo  q quit";
        let text = match &self.error {
            _ if self.typing => Line::from(vec![
                Span::raw(format!("/{}▏", self.filter)),
                Span::styled("   enter keep  esc clear", Style::default().fg(Color::DarkGray)),
            ]),
            Some(e) => Line::from(format!("error: {e}")).red(),
            None if self.status.is_empty() => Line::from(keys).dark_gray(),
            None => Line::from(vec![
                Span::raw(self.status.clone()),
                Span::styled(format!("   {keys}"), Style::default().fg(Color::DarkGray)),
            ]),
        };
        frame.render_widget(text, area);
    }
}

fn is_synth(d: &Definition) -> bool {
    d.name.starts_with('«') && d.name.ends_with('»')
}

/// Definitions as a preorder tree: roots by (file, ordinal), children under their parent.
/// Synthetic `«use_declaration:N»` opaques are not review targets.
fn tree_rows(defs: &[Definition]) -> Vec<(usize, usize)> {
    let by_id: HashMap<&str, usize> = defs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (i, d) in defs.iter().enumerate() {
        if is_synth(d) {
            continue;
        }
        let parent = d
            .parent
            .as_deref()
            .and_then(|p| by_id.get(p).copied())
            .filter(|&pi| !is_synth(&defs[pi]));
        children.entry(parent).or_default().push(i);
    }
    for v in children.values_mut() {
        v.sort_by(|a, b| (&defs[*a].file, defs[*a].ordinal).cmp(&(&defs[*b].file, defs[*b].ordinal)));
    }
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = children
        .get(&None)
        .map(|v| v.iter().rev().map(|i| (*i, 0)).collect())
        .unwrap_or_default();
    while let Some((i, depth)) = stack.pop() {
        out.push((i, depth));
        if let Some(kids) = children.get(&Some(i)) {
            stack.extend(kids.iter().rev().map(|k| (*k, depth + 1)));
        }
    }
    out
}

fn entity_touch_line(entity: &EntityTouch) -> Line<'static> {
    let (text, style) = match &entity.touch {
        Touch::Added => ("added".into(), Style::default().fg(Color::Green)),
        Touch::Removed => ("removed".into(), Style::default().fg(Color::Red)),
        Touch::Renamed { from, to } => (format!("renamed {from} → {to}"), Style::default().fg(Color::Cyan)),
        Touch::Moved { .. } => ("moved".into(), Style::default().fg(Color::Cyan)),
        Touch::Relocated { from, to } => (
            format!("relocated {}#{} → {}#{}", from.0, from.1, to.0, to.1),
            Style::default().fg(Color::Cyan),
        ),
        Touch::Edited { observed: Some(svc_core::ObservedClass::BindingChanging) } => {
            ("edited: binding-changing".into(), Style::default().fg(Color::Red))
        }
        Touch::Edited { observed } => (format!("edited: {}", class_name(*observed)), Style::default().fg(Color::Green)),
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(text, style),
        Span::raw(format!("  {}", entity.name)),
    ])
}

fn op_log_line(op: &OpOut) -> Line<'static> {
    let who = op.subject.clone().unwrap_or_default();
    let desc = describe_op(&op.op);
    let text = if who.is_empty() {
        desc
    } else {
        format!("{who}: {desc}")
    };
    let style = if op.flagged {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    // A named checkout's op says so; the default checkout's says nothing.
    let from = op.workspace.as_deref().map(|w| format!("  [{w}]")).unwrap_or_default();
    Line::from(vec![
        Span::styled(format!("#{:<3} ", op.ix.0), Style::default().fg(Color::DarkGray)),
        Span::styled(text, style),
        Span::styled(from, Style::default().fg(Color::DarkGray)),
    ])
}

fn blame_line(e: &BlameEntry) -> Line<'static> {
    let touch = match &e.touch {
        Touch::Added => "added".to_string(),
        Touch::Removed => "removed".to_string(),
        Touch::Renamed { from, to } => format!("renamed {from} → {to}"),
        Touch::Moved { .. } => "moved".to_string(),
        Touch::Relocated { from, to } => format!("relocated {}#{} → {}#{}", from.0, from.1, to.0, to.1),
        Touch::Edited { observed } => format!("edited: {}", class_name(*observed)),
    };
    let op = match &e.op {
        Op::Rename { .. } => String::new(),
        other => format!(" — {}", describe_op(other)),
    };
    let style = match &e.touch {
        Touch::Edited { observed: Some(svc_core::ObservedClass::BindingChanging) } => Style::default().fg(Color::Red),
        Touch::Edited { .. } => Style::default().fg(Color::Green),
        _ => Style::default(),
    };
    Line::from(vec![
        Span::styled(format!("#{:<3} ", e.ix.0), Style::default().fg(Color::DarkGray)),
        Span::styled(touch, style),
        Span::raw(op),
    ])
}

fn queue_line(q: &QueueItem) -> Line<'static> {
    match q {
        QueueItem::Ask { ask, tool, entity, intent, answered, .. } => {
            let (tag, style) = match (ask.is_some(), answered) {
                (true, _) => ("ASK", Style::default().fg(Color::Yellow).bold()),
                (false, Some(true)) => ("…", Style::default().fg(Color::Yellow)),
                (false, _) => ("✗", Style::default().fg(Color::Red)),
            };
            let tail = match (ask.is_some(), answered) {
                (true, _) => "  a: allow  r: reject".to_string(),
                (false, Some(true)) => "  allowed — awaiting verdict".to_string(),
                (false, _) => "  rejected".to_string(),
            };
            Line::from(vec![
                Span::styled(format!("[{tag}] "), style),
                Span::raw(format!("{tool} {entity} declared {intent}")),
                Span::styled(tail, Style::default().fg(Color::DarkGray)),
            ])
        }
        QueueItem::Edit { op, entity } => {
            let (tag, style) = if op.flagged {
                ("✗", Style::default().fg(Color::Red).bold())
            } else {
                ("✓", Style::default().fg(Color::Green).bold())
            };
            Line::from(vec![
                Span::styled(format!("[{tag}] "), style),
                Span::raw(format!(
                    "edit-def {entity}  declared {} / observed {}",
                    op.declared.as_ref().map(intent_name).unwrap_or_default(),
                    class_name(op.observed)
                )),
            ])
        }
        QueueItem::Binding { line } => Line::from(vec![
            Span::styled("[!] ".to_string(), Style::default().fg(Color::Red).bold()),
            Span::raw(line.clone()),
        ]),
    }
}

/// Unified-diff rows for an edit-def: removed red, added green, context dim; long diffs
/// are cut with a count so the queue stays a queue.
fn edit_diff_lines(before: &str, after: &str) -> Vec<Line<'static>> {
    const MAX: usize = 40;
    let diff = similar::TextDiff::from_lines(before, after);
    let mut out = Vec::new();
    let mut hidden = 0usize;
    for hunk in diff.unified_diff().context_radius(2).iter_hunks() {
        for change in hunk.iter_changes() {
            let text = change.value().trim_end_matches('\n').to_string();
            let line = match change.tag() {
                similar::ChangeTag::Delete => Line::from(format!("    - {text}")).red(),
                similar::ChangeTag::Insert => Line::from(format!("    + {text}")).green(),
                similar::ChangeTag::Equal => Line::from(format!("      {text}")).dark_gray(),
            };
            if out.len() < MAX {
                out.push(line);
            } else {
                hidden += 1;
            }
        }
    }
    if hidden > 0 {
        out.push(Line::from(format!("      … {hidden} more lines")).dark_gray());
    }
    if out.is_empty() {
        out.push(Line::from("      (no textual change)").dark_gray());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use svc_core::EntityId;
    use svc_agent::AgentConfig;
    use svc_core::{ChangeId, Intent, ObservedClass, OpIx, SnapshotId};
    use tokio::sync::mpsc;

    fn app_with_agent() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
        // Nothing here shells out: `Svc` is only consulted by `refresh`, which tests never call.
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        let (tx, rx) = mpsc::unbounded_channel();
        app.agent = Some(AgentLink {
            commands: tx,
            task: "rename read to read_file".into(),
            preseed: false,
            running: false,
            tool_titles: Default::default(),
        });
        (app, rx)
    }

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn change(message: &str, current: bool) -> ChangeOut {
        let change = ChangeId::new();
        ChangeOut {
            change,
            short: change.short(),
            snapshot: SnapshotId::of(&message),
            message: message.into(),
            branches: Vec::new(),
            current,
        }
    }

    fn prompt_text(cmd: AgentCommand) -> String {
        match cmd {
            AgentCommand::Prompt(t) => t,
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn ready_sends_the_task_as_the_first_prompt() {
        let (mut app, mut rx) = app_with_agent();
        app.on_agent_event(AgentEvent::Ready { session_id: "s1".into() });
        assert!(app.agent.as_ref().unwrap().running);
        assert_eq!(prompt_text(rx.try_recv().unwrap()), "rename read to read_file");
        assert!(rx.try_recv().is_err(), "exactly one prompt");
    }

    #[test]
    fn preseed_puts_the_entity_list_in_the_first_prompt() {
        let (mut app, mut rx) = app_with_agent();
        app.agent.as_mut().unwrap().preseed = true;
        app.defs = vec![Definition {
            id: "01a0-read".into(),
            name: "read".into(),
            kind: svc_core::Kind::Fn,
            file: "src/main.rs".into(),
            parent: None,
            ordinal: 3,
        }];
        app.on_agent_event(AgentEvent::Ready { session_id: "s1".into() });
        let prompt = prompt_text(rx.try_recv().unwrap());
        assert!(prompt.starts_with("rename read to read_file\n\nDefinitions in this repository"), "{prompt}");
        assert!(prompt.contains("Fn read in src/main.rs (id 01a0-read)"), "{prompt}");
    }

    #[test]
    fn a_tool_call_touches_its_entity_and_is_logged() {
        let (mut app, _rx) = app_with_agent();
        app.on_agent_event(AgentEvent::ToolCall {
            id: "c1".into(),
            title: "rename".into(),
            status: "pending".into(),
            raw_input: Some(json!({"entity": "read", "new_name": "read_file"})),
        });
        assert!(app.touched.contains("read"));
        assert_eq!(app.status, "agent → rename");
        assert!(app.log.last().unwrap().starts_with("tool_call rename"));
        app.on_agent_event(AgentEvent::ToolCallUpdate {
            id: "c1".into(),
            title: None,
            status: Some("completed".into()),
            raw_output: None,
        });
        assert!(app.dirty, "a completed call refreshes the panes");
    }

    #[test]
    fn end_turn_keeps_the_session_and_p_continues_it() {
        let (mut app, mut rx) = app_with_agent();
        app.on_agent_event(AgentEvent::Ready { session_id: "s1".into() });
        let _ = rx.try_recv();
        app.on_agent_event(AgentEvent::Stopped { reason: "end_turn".into() });
        assert!(!app.agent.as_ref().unwrap().running);
        assert!(!app.should_quit, "the TUI stays up after the agent's turn ends");
        assert!(rx.try_recv().is_err(), "no Quit is sent on end_turn");
        assert!(app.status.contains("p: continue"));

        app.handle_key(&key('p'));
        let follow_up = prompt_text(rx.try_recv().unwrap());
        assert!(follow_up.starts_with("Continue"));
        assert!(follow_up.contains("rename read to read_file"));
        assert!(app.agent.as_ref().unwrap().running);

        app.handle_key(&key('p'));
        assert_eq!(app.status, "agent is still running");
        assert!(rx.try_recv().is_err(), "no second prompt while a turn is running");

        app.handle_key(&key('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn edit_def_ops_become_verdicts_named_from_the_subject() {
        let (mut app, _rx) = app_with_agent();
        let op = |flagged: bool, subject: &str| OpOut {
            ix: OpIx(1),
            op: Op::EditDef { id: EntityId::new(), definition: String::new(), intent: Intent::Refactor },
            declared: Some(Intent::Refactor),
            observed: Some(if flagged { ObservedClass::BindingChanging } else { ObservedClass::BindingPreserving }),
            flagged,
            at: 0,
            group: None,
            root_after: SnapshotId::of(&()),
            subject: Some(subject.into()),
            workspace: None,
        };
        let rename = OpOut {
            op: Op::Rename { id: EntityId::new(), new: "read_file".into() },
            ..op(false, "read")
        };
        // Newest first, as `svc log` returns them.
        app.rebuild_queue(&[op(true, "validate"), rename, op(false, "load")], &[]);
        let names: Vec<(&str, bool)> = app
            .queue
            .iter()
            .filter_map(|q| match q {
                QueueItem::Edit { op, entity } => Some((entity.as_str(), op.flagged)),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec![("validate", true), ("load", false)], "only edit-defs queue, in log order");
        assert!(app.queue.len() == 2, "renames never queue");
        assert_eq!(app.queue_state.selected(), Some(0));
    }

    /// The ask path end to end against the scripted ACP agent from `svc-agent`'s tests:
    /// the ask lands in the queue with the tool's own arguments, `a` answers it, the agent
    /// sees the allow and finishes, and the TUI is still up afterwards.
    #[test]
    fn the_real_ask_is_answered_from_the_queue() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../svc-agent/tests/fake_agent.mjs");
        let config = AgentConfig::command("node", vec![script.display().to_string()], Path::new("/tmp"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let driver = tokio::spawn(svc_agent::run(config, ev_tx, cmd_rx, false));
            app.agent = Some(AgentLink {
                commands: cmd_tx,
                task: "edit validate".into(),
                preseed: false,
                running: false,
                tool_titles: Default::default(),
            });
            let mut answered = false;
            let mut stopped = None;
            while let Some(ev) = tokio::time::timeout(Duration::from_secs(20), ev_rx.recv()).await.expect("agent went quiet") {
                let is_stop = matches!(ev, AgentEvent::Stopped { .. });
                if let AgentEvent::Stopped { reason } = &ev {
                    stopped = Some(reason.clone());
                }
                app.on_agent_event(ev);
                if !answered && app.queue.iter().any(QueueItem::pending) {
                    let QueueItem::Ask { tool, entity, intent, .. } = &app.queue[0] else { panic!("ask first") };
                    assert_eq!((tool.as_str(), entity.as_str(), intent.as_str()), ("edit_def", "validate", "refactor"));
                    assert!(matches!(app.focus, Pane::Queue), "an ask pulls focus to the queue");
                    app.handle_key(&key('a'));
                    assert_eq!(app.status, "allowed edit-def validate");
                    answered = true;
                }
                if is_stop {
                    break;
                }
            }
            assert!(answered, "the ask reached the queue");
            assert_eq!(stopped.as_deref(), Some("end_turn"));
            assert!(app.log.iter().any(|l| l == "tool_call_update call-1 completed"), "{:?}", app.log);
            assert!(app.log.iter().any(|l| l == "agent: done"), "the agent saw the allow: {:?}", app.log);
            assert!(!app.should_quit);
            assert!(!app.agent.as_ref().unwrap().running);
            let _ = app.agent.as_ref().unwrap().commands.send(AgentCommand::Quit);
            let _ = tokio::time::timeout(Duration::from_secs(5), driver).await;
        });
    }

    #[test]
    fn init_blame_names_the_new_change_not_just_added() {
        let change = ChangeId::new();
        let entry = BlameEntry {
            ix: OpIx(0),
            change,
            op: Op::New { change },
            touch: Touch::Added,
            at: 0,
        };
        let text: String = blame_line(&entry)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("added"), "{text}");
        assert!(text.contains("new change"), "{text}");
    }

    fn def(id: &str, name: &str, ordinal: u32) -> Definition {
        Definition {
            id: id.into(),
            name: name.into(),
            kind: svc_core::Kind::Fn,
            file: "src/main.rs".into(),
            parent: None,
            ordinal,
        }
    }

    #[test]
    fn moving_the_tree_does_not_spawn_show_def_on_each_j() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        app.defs = vec![def("id-read", "read", 0), def("id-load", "load", 1)];
        app.rows = tree_rows(&app.defs);
        app.tree_state.select(Some(0));
        app.mode = ViewMode::Entities;
        app.events_for = Some("entity:id-read".into());
        app.events = vec![Line::from("stale")];
        app.dirty = false;
        app.handle_key(&key('j'));
        assert_eq!(app.tree_state.selected(), Some(1));
        assert!(
            app.events_for.is_none(),
            "right pane waits until the selection settles"
        );
        assert!(app.need_draw);
        app.pump();
        assert!(
            app.events_for.is_none(),
            "pump must not shell out in the same millisecond as j"
        );
    }

    #[test]
    fn revisions_are_default_and_secondary_views_return_to_them() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));

        assert_eq!(app.mode, ViewMode::Revisions);
        app.handle_key(&key('e'));
        assert_eq!(app.mode, ViewMode::Entities);
        app.handle_key(&key('h'));
        assert_eq!(app.mode, ViewMode::Revisions);
        app.handle_key(&key('o'));
        assert_eq!(app.mode, ViewMode::Oplog);
        app.handle_key(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.mode, ViewMode::Revisions);
        assert!(!app.should_quit);
    }

    #[test]
    fn default_frame_renders_revision_list_at_eighty_columns() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/repo")));
        app.changes = vec![change("refactor config", true), change("add loader", false)];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal.draw(|frame| app.render(frame)).unwrap();

        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("revisions (2)"), "{text}");
        assert!(text.contains("refactor config"), "{text}");
        assert!(text.contains("review queue"), "{text}");
    }

    #[test]
    fn moving_revisions_invalidates_the_change_preview() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        app.changes = vec![change("current", true), change("other", false)];
        app.revision_state.select(Some(0));
        app.events_for = Some(format!("change:{}", app.changes[0].change));

        app.handle_key(&key('j'));

        assert_eq!(app.revision_state.selected(), Some(1));
        assert!(app.events_for.is_none());
    }

    #[test]
    fn a_duplicate_resize_does_not_mark_the_frame_dirty() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        app.need_draw = false;
        app.handle_key(&Event::Resize(80, 24));
        assert!(app.need_draw);
        app.need_draw = false;
        app.handle_key(&Event::Resize(80, 24));
        assert!(!app.need_draw, "same size is not a new frame");
        app.handle_key(&Event::Resize(120, 40));
        assert!(app.need_draw);
    }

    #[test]
    fn synthetic_use_items_are_not_tree_rows() {
        let defs = vec![
            def("id-use", "«use_declaration:0»", 0),
            def("id-load", "load", 1),
        ];
        let rows = tree_rows(&defs);
        assert_eq!(rows.len(), 1);
        assert_eq!(defs[rows[0].0].name, "load");
    }

    #[test]
    fn first_pick_jumps_to_the_latest_logged_entity() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        app.defs = vec![
            def("id-use", "«use_declaration:0»", 0),
            def("id-parse", "parse_cfg", 1),
            def("id-load", "load", 2),
        ];
        app.rows = tree_rows(&app.defs);
        app.tree_state.select(Some(0));
        app.ops = vec![OpOut {
            ix: OpIx(6),
            op: Op::EditDef {
                id: EntityId::new(),
                definition: String::new(),
                intent: Intent::Feature,
            },
            declared: Some(Intent::Feature),
            observed: Some(ObservedClass::BindingPreserving),
            flagged: false,
            at: 0,
            group: None,
            root_after: SnapshotId::of(&()),
            subject: Some("load".into()),
            workspace: None,
        }];
        app.pick_story_entity();
        let i = app.tree_state.selected().unwrap();
        assert_eq!(app.defs[app.rows[i].0].name, "load");
        app.pick_story_entity();
        let j = app.tree_state.selected().unwrap();
        assert_eq!(i, j, "later refreshes must not steal the caret");
    }

    #[test]
    fn another_process_publishing_to_the_store_makes_the_next_probe_refresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".svc")).unwrap();
        let store = dir.path().join(".svc/store.redb");
        std::fs::write(&store, b"v1").unwrap();
        let mut app = App::new(Svc::new(PathBuf::from("svc"), dir.path().to_path_buf()));
        app.refresh(); // no svc binary here: the panes error, the store mtime is still recorded
        assert!(app.store_seen.is_some());
        app.store_probe_at = Instant::now();
        assert!(!app.store_moved(), "nothing published");
        assert!(!app.store_moved(), "and the probe is rate-limited, not re-run per pump");

        // Another checkout publishes: redb rewrites the file.
        let later = SystemTime::now() + Duration::from_secs(2);
        std::fs::File::options().write(true).open(&store).unwrap().set_modified(later).unwrap();
        app.store_probe_at = Instant::now();
        assert!(app.store_moved(), "the next probe sees it");
        app.refresh(); // as pump would: the refresh re-records the mtime
        app.store_probe_at = Instant::now();
        assert!(!app.store_moved(), "settled after the refresh");
    }

    #[test]
    fn slash_filters_the_tree_by_name_or_file_and_esc_clears_it() {
        let mut app = App::new(Svc::new(PathBuf::from("svc"), PathBuf::from("/nonexistent")));
        app.defs = vec![def("a", "read", 0), def("b", "parse", 1), def("c", "Config", 2)];
        app.defs[2].file = "src/lib.rs".into();
        app.rows = tree_rows(&app.defs);
        let code = |app: &mut App, c: KeyCode| app.handle_key(&Event::Key(KeyEvent::new(c, KeyModifiers::NONE)));
        code(&mut app, KeyCode::Char('e')); // the filter is the entity view's
        code(&mut app, KeyCode::Char('/'));
        for c in "PAR".chars() {
            code(&mut app, KeyCode::Char(c));
        }
        assert!(app.typing);
        assert_eq!(app.rows.len(), 1, "case-insensitive substring of the name");
        assert_eq!(app.defs[app.rows[0].0].name, "parse");
        assert_eq!(app.tree_state.selected(), Some(0));
        code(&mut app, KeyCode::Enter);
        assert!(!app.typing && app.rows.len() == 1, "enter keeps the filter");
        code(&mut app, KeyCode::Char('j'));
        assert!(!app.should_quit, "keys are the tree's again");
        code(&mut app, KeyCode::Esc);
        assert!(app.filter.is_empty() && app.rows.len() == 3 && !app.should_quit, "esc clears the filter first");
        code(&mut app, KeyCode::Char('/'));
        for c in "lib".chars() {
            code(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.rows.len(), 1, "or of the file");
        assert_eq!(app.defs[app.rows[0].0].name, "Config");
        code(&mut app, KeyCode::Esc);
        assert!(app.filter.is_empty() && app.mode == ViewMode::Entities, "first esc clears the filter");
        code(&mut app, KeyCode::Esc);
        assert!(app.mode == ViewMode::Revisions && !app.should_quit, "then esc returns to revisions");
        code(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit, "q quits");
    }

    #[test]
    fn an_edit_diff_is_removed_red_added_green_context_dim_and_cut_with_a_count() {
        let before = "fn validate(c: &Config) {\n    if c.retries > 3 {\n        panic!()\n    }\n}\n";
        let after = "fn validate(c: &Config) {\n    check_retries(c)\n}\n";
        let lines = edit_diff_lines(before, after);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text.iter().any(|l| l.starts_with("    - ") && l.contains("retries > 3")), "{text:?}");
        assert!(text.iter().any(|l| l.starts_with("    + ") && l.contains("check_retries")), "{text:?}");
        assert!(text.iter().any(|l| l.starts_with("      fn validate")), "context kept: {text:?}");
        assert_eq!(edit_diff_lines("same\n", "same\n").len(), 1, "no textual change");
        let big_before = (0..100).map(|i| format!("l{i}\n")).collect::<String>();
        let big_after = (0..100).map(|i| format!("m{i}\n")).collect::<String>();
        let cut = edit_diff_lines(&big_before, &big_after);
        assert_eq!(cut.len(), 41);
        assert!(cut.last().unwrap().to_string().contains("more lines"));
    }

    #[test]
    fn a_busy_checkout_is_retried_quietly_not_shown_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("svc");
        std::fs::write(&fake, "#!/bin/sh\necho '{\"error\":\"checkout busy: another svc session held it\"}' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut app = App::new(Svc::new(fake, dir.path().to_path_buf()));
        app.refresh();
        assert!(app.dirty, "still to be read");
        assert!(app.error.is_none(), "not an error to show");
        assert!(app.retry_at > Instant::now(), "and not before a pause");
        app.pump();
        assert!(app.dirty, "pump waits out the pause instead of spawning again");
    }
}
