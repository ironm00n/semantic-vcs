use std::collections::{HashMap, HashSet};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::Value;
use svc_agent::{AgentCommand, AgentEvent, PermissionAsk};
use svc_core::{Conflict, Op};
use svc_repo::{BlameEntry, ConflictOut, OpOut, Touch};
use tokio::sync::mpsc::UnboundedSender;

use crate::data::{Definition, Svc, class_name, conflict_line, describe_op, intent_name, kind_glyph};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Queue,
}

/// SPEC §7: every `edit_def` queues (green when declared and observed agree, red when they
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
    pub running: bool,
    pub tool_titles: HashMap<String, (String, Value)>,
}

pub struct App {
    pub svc: Svc,
    pub defs: Vec<Definition>,
    pub rows: Vec<(usize, usize)>,
    pub tree_state: ListState,
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
    pub should_quit: bool,
    pub log: Vec<String>,
}

impl App {
    pub fn new(svc: Svc) -> Self {
        let mut app = Self {
            svc,
            defs: Vec::new(),
            rows: Vec::new(),
            tree_state: ListState::default(),
            events: Vec::new(),
            events_for: None,
            queue: Vec::new(),
            queue_state: ListState::default(),
            expanded: HashSet::new(),
            focus: Pane::Tree,
            agent: None,
            touched: HashSet::new(),
            status: String::new(),
            error: None,
            dirty: true,
            should_quit: false,
            log: Vec::new(),
        };
        app.tree_state.select(Some(0));
        app
    }

    /// Re-read everything from `svc --json`. Pending asks survive; verdict rows replace answered asks.
    pub fn refresh(&mut self) {
        self.dirty = false;
        match self.svc.list_defs() {
            Ok(defs) => {
                self.defs = defs;
                self.rows = tree_rows(&self.defs);
                let n = self.rows.len();
                if n == 0 {
                    self.tree_state.select(None);
                } else if self.tree_state.selected().is_none_or(|s| s >= n) {
                    self.tree_state.select(Some(n.saturating_sub(1)));
                }
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        let log = self.svc.log().unwrap_or_default();
        let conflicts = self.svc.conflicts().unwrap_or_default();
        self.rebuild_queue(&log, &conflicts);
        self.events_for = None;
        self.load_events();
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
                let entity = self.name_of(&id.to_string());
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

    pub fn name_of(&self, id: &str) -> String {
        self.defs
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| id.chars().take(8).collect())
    }

    fn selected_def(&self) -> Option<&Definition> {
        self.tree_state
            .selected()
            .and_then(|s| self.rows.get(s))
            .map(|(i, _)| &self.defs[*i])
    }

    pub fn load_events(&mut self) {
        let Some(def) = self.selected_def().cloned() else {
            self.events.clear();
            return;
        };
        if self.events_for.as_deref() == Some(def.id.as_str()) {
            return;
        }
        self.events_for = Some(def.id.clone());
        self.events = match self.svc.blame(&def.id) {
            Ok(entries) => entries.iter().map(blame_line).collect(),
            Err(e) => vec![Line::from(format!("blame failed: {e}")).red()],
        };
    }

    pub fn handle_key(&mut self, event: &Event) {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Pane::Tree => Pane::Queue,
                    Pane::Queue => Pane::Tree,
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Enter => {
                if self.focus == Pane::Queue {
                    if let Some(i) = self.queue_state.selected() {
                        if !self.expanded.remove(&i) {
                            self.expanded.insert(i);
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

    fn move_sel(&mut self, delta: i32) {
        let (state, len) = match self.focus {
            Pane::Tree => (&mut self.tree_state, self.rows.len()),
            Pane::Queue => (&mut self.queue_state, self.queue.len()),
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
        state.select(Some(next));
        if self.focus == Pane::Tree {
            self.load_events();
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
        match ev {
            AgentEvent::Ready { session_id } => {
                self.status = format!("agent session {session_id}");
                if let Some(agent) = &mut self.agent {
                    agent.running = true;
                    let _ = agent.commands.send(AgentCommand::Prompt(agent.task.clone()));
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
                self.status = format!("agent finished: {reason}");
                if let Some(agent) = &mut self.agent {
                    agent.running = false;
                    let _ = agent.commands.send(AgentCommand::Quit);
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
            " svc review — {} — queue {} ({} pending) ",
            self.svc.root.display(),
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
        self.render_tree(frame, left);
        self.render_events(frame, right);
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
            .block(self.border(Pane::Tree, format!(" entities ({}) ", self.defs.len())))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.tree_state);
    }

    fn render_events(&self, frame: &mut Frame, area: Rect) {
        let name = self.selected_def().map(|d| d.name.clone()).unwrap_or_default();
        let mut lines = self.events.clone();
        if lines.is_empty() {
            lines.push(Line::from("no events").dark_gray());
        }
        if let Some(agent) = &self.agent {
            lines.push(Line::from(""));
            lines.push(Line::from(format!("agent{}: {}", if agent.running { " (running)" } else { "" }, agent.task)).bold());
            for l in self.log.iter().rev().take(8).rev() {
                lines.push(Line::from(l.chars().take(area.width as usize).collect::<String>()).dark_gray());
            }
        }
        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(format!(" events — {name} ")))
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
                    lines.extend(queue_detail(q));
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
        let keys = "j/k move  tab pane  enter expand  a/r allow/reject  u undo  c cancel  q quit";
        let text = match &self.error {
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

/// Definitions as a preorder tree: roots by (file, ordinal), children under their parent.
fn tree_rows(defs: &[Definition]) -> Vec<(usize, usize)> {
    let by_id: HashMap<&str, usize> = defs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (i, d) in defs.iter().enumerate() {
        let parent = d.parent.as_deref().and_then(|p| by_id.get(p).copied());
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
        Op::Rename { .. } | Op::New { .. } => String::new(),
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

fn queue_detail(q: &QueueItem) -> Vec<Line<'static>> {
    match q {
        QueueItem::Ask { definition, .. } => definition
            .lines()
            .take(5)
            .map(|l| Line::from(format!("      {l}")).dark_gray())
            .collect(),
        QueueItem::Edit { op, .. } => {
            let mut v = vec![Line::from(format!("      op #{}  at {}", op.ix.0, op.at)).dark_gray()];
            if let Op::EditDef { definition, .. } = &op.op {
                v.extend(definition.lines().take(5).map(|l| Line::from(format!("      {l}")).dark_gray()));
            }
            v
        }
        QueueItem::Binding { .. } => vec![Line::from("      fix the code, or `svc resolve <n> --accept`").dark_gray()],
    }
}
