use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write, stdout};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{DefaultTerminal, Frame};
use serde::{Deserialize, Serialize};

const POLL: Duration = Duration::from_secs(2);
const CARD_LINES: u16 = 3;
const MIN_PANEL: u16 = 3;
const WHEEL_STEP: u16 = 3;
/// Safety net for the event watcher: refresh this often even if Herdr stays quiet
/// (terminal titles and working directories change without an event).
const HEARTBEAT: Duration = Duration::from_secs(30);
/// Events arrive in bursts; wait this long for the burst to end before refreshing.
const SETTLE: Duration = Duration::from_millis(100);

mod orca;

#[derive(Clone, Copy, Default, PartialEq)]
enum Source {
    #[default]
    Herdr,
    Orca,
}

#[derive(Deserialize, Clone)]
struct Agent {
    agent: String,
    agent_status: String,
    cwd: String,
    pane_id: String,
    workspace_id: String,
    #[serde(default)]
    terminal_title_stripped: String,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    label: String,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    agents: Vec<Agent>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
}

enum Reply {
    Ok(String, Vec<(Agent, String)>),
    Err(String, String),
    Msg(String),
}

struct Panel {
    label: String,
    source: Source,
    /// `--machine` argument; `None` for the local session.
    machine: Option<String>,
    agents: Vec<(Agent, String)>,
    /// Pane IDs pinned to the top of this panel.
    pinned: HashSet<String>,
    error: Option<String>,
    scroll: u16,
    /// Explicit height set by dragging; `None` sizes to content.
    height: Option<u16>,
    rect: Rect,
}

impl Panel {
    fn new(label: &str, machine: Option<&str>) -> Self {
        Self {
            label: label.into(),
            source: Source::Herdr,
            machine: machine.map(String::from),
            agents: vec![],
            pinned: HashSet::new(),
            error: None,
            scroll: 0,
            height: None,
            rect: Rect::default(),
        }
    }

    fn header_lines(&self) -> u16 {
        self.error.is_some() as u16
    }

    fn content_lines(&self) -> u16 {
        (self.agents.len() as u16 * CARD_LINES + self.header_lines()).max(1)
    }

    fn sorted(&self) -> Vec<&(Agent, String)> {
        let mut v: Vec<_> = self.agents.iter().collect();
        v.sort_by_key(|(a, _)| (!self.pinned.contains(&a.pane_id), rank(&a.agent_status)));
        v
    }

    fn inner(&self) -> Rect {
        self.rect.inner(Margin::new(1, 1))
    }

    fn max_scroll(&self) -> u16 {
        self.content_lines().saturating_sub(self.inner().height)
    }
}

struct App {
    panels: Vec<Panel>,
    /// Panel last touched by the mouse; keyboard scrolling falls back to the busiest panel.
    hover: Option<usize>,
    drag: Option<usize>,
    msg: Option<(String, Instant)>,
    menu: Option<Menu>,
}

/// Right-click menu for one agent card.
struct Menu {
    panel: usize,
    pane_id: String,
    pinned: bool,
    /// Top-left corner, clamped to the screen when drawn.
    x: u16,
    y: u16,
    rect: Rect,
}

impl Menu {
    fn items(&self) -> [&'static str; 1] {
        [if self.pinned { "Unpin" } else { "Pin" }]
    }
}

fn herdr(machine: Option<&str>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("herdr");
    if let Some(m) = machine {
        cmd.args(["--machine", m]);
    }
    let out = cmd.args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fetch(machine: Option<&str>) -> Result<Vec<(Agent, String)>> {
    let raw = herdr(machine, &["api", "snapshot"])?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    let snap: Snapshot = serde_json::from_value(v["result"]["snapshot"].clone())?;
    let names: HashMap<_, _> = snap
        .workspaces
        .into_iter()
        .map(|w| (w.workspace_id, w.label))
        .collect();
    Ok(snap
        .agents
        .into_iter()
        .map(|a| {
            let ws = names.get(&a.workspace_id).cloned().unwrap_or_default();
            (a, ws)
        })
        .collect())
}

fn remote_machines() -> Vec<String> {
    herdr(None, &["machine", "list", "--json"])
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .map(|ms| {
            ms.iter()
                .filter(|m| m["enabled"].as_bool().unwrap_or(false))
                .filter_map(|m| m["label"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn spawn_poller(source: Source, label: String, machine: Option<String>, tx: mpsc::Sender<Reply>) {
    thread::spawn(move || {
        loop {
            let result = match source {
                Source::Herdr => fetch(machine.as_deref()),
                Source::Orca => orca::fetch(),
            };
            let reply = match result {
                Ok(a) => Reply::Ok(label.clone(), a),
                Err(e) => Reply::Err(label.clone(), e.to_string()),
            };
            if tx.send(reply).is_err() {
                return;
            }
            thread::sleep(POLL);
        }
    });
}

/// Where Herdr's socket API lives, the same way the `herdr` CLI finds it.
fn socket_path() -> Option<PathBuf> {
    std::env::var_os("HERDR_SOCKET_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/herdr/herdr.sock"))
        })
}

/// Open the event stream for the local session. `None` means the socket is
/// unavailable and the caller should fall back to polling.
fn subscribe(panes: &[String]) -> Option<BufReader<UnixStream>> {
    let mut stream = UnixStream::connect(socket_path()?).ok()?;
    let mut subscriptions: Vec<serde_json::Value> = [
        "pane.created",
        "pane.closed",
        "pane.exited",
        "pane.agent_detected",
        "workspace.created",
        "workspace.closed",
        "workspace.renamed",
    ]
    .iter()
    .map(|t| serde_json::json!({ "type": t }))
    .collect();
    subscriptions.extend(
        panes
            .iter()
            .map(|p| serde_json::json!({ "type": "pane.agent_status_changed", "pane_id": p })),
    );
    let request = serde_json::json!({
        "id": "bistro",
        "method": "events.subscribe",
        "params": { "subscriptions": subscriptions },
    });
    writeln!(stream, "{request}").ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut reader = BufReader::new(stream);
    let mut ack = String::new();
    reader.read_line(&mut ack).ok()?;
    ack.contains("subscription_started").then_some(reader)
}

/// Block until Herdr reports a change, the stream closes, or the heartbeat expires.
fn wait_for_event(reader: &mut BufReader<UnixStream>) {
    let mut line = String::new();
    let _ = reader.get_ref().set_read_timeout(Some(HEARTBEAT));
    if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
        return;
    }
    // Let the rest of a burst arrive so it costs one refresh, not many.
    let _ = reader.get_ref().set_read_timeout(Some(SETTLE));
    while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {}
}

/// Refresh the local session when Herdr says something changed, instead of on a timer.
///
/// Subscribes before fetching so no change can slip between the two. Agent
/// status events are per pane, so whenever the set of agent panes changes the
/// subscription is rebuilt. Falls back to polling when the socket is unavailable.
fn spawn_watcher(label: String, tx: mpsc::Sender<Reply>) {
    thread::spawn(move || {
        let mut panes: Vec<String> = vec![];
        loop {
            let stream = subscribe(&panes);
            let result = fetch(None);
            let current = match &result {
                Ok(agents) => {
                    let mut ids: Vec<String> =
                        agents.iter().map(|(a, _)| a.pane_id.clone()).collect();
                    ids.sort();
                    Some(ids)
                }
                Err(_) => None,
            };
            let reply = match result {
                Ok(a) => Reply::Ok(label.clone(), a),
                Err(e) => Reply::Err(label.clone(), e.to_string()),
            };
            if tx.send(reply).is_err() {
                return;
            }
            if let Some(ids) = current
                && ids != panes
            {
                panes = ids;
                continue;
            }
            match stream {
                Some(mut reader) => wait_for_event(&mut reader),
                None => thread::sleep(POLL),
            }
        }
    });
}

fn focus_command(source: Source, machine: Option<&str>, pane: &str) -> Result<()> {
    match source {
        Source::Herdr => herdr(machine, &["agent", "focus", pane]).map(|_| ()),
        Source::Orca => orca::focus(pane),
    }
}

fn spawn_focus(
    source: Source,
    machine: Option<String>,
    label: String,
    agent: &Agent,
    tx: mpsc::Sender<Reply>,
) {
    let pane = agent.pane_id.clone();
    let kind = agent.agent.clone();
    thread::spawn(move || {
        let msg = match focus_command(source, machine.as_deref(), &pane) {
            Ok(_) => format!("focused {kind} {pane} on {label}"),
            Err(e) => format!("focus failed: {e}"),
        };
        let _ = tx.send(Reply::Msg(msg));
    });
}

fn status_style(status: &str) -> (Color, &'static str) {
    match status {
        "working" => (Color::Yellow, "●"),
        "blocked" => (Color::Red, "▲"),
        "done" => (Color::Green, "✔"),
        "idle" => (Color::DarkGray, "○"),
        _ => (Color::Gray, "?"),
    }
}

fn rank(status: &str) -> u8 {
    match status {
        "blocked" => 0,
        "done" => 1,
        "working" => 2,
        "idle" => 3,
        _ => 4,
    }
}

fn short_cwd(cwd: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if cwd.starts_with(&h) => format!("~{}", &cwd[h.len()..]),
        _ => cwd.to_string(),
    }
}

/// Index of the panel that absorbs leftover height: the busiest panel without an
/// explicit height, falling back to the last panel.
fn fill_index(panels: &[Panel]) -> usize {
    panels
        .iter()
        .enumerate()
        .filter(|(_, p)| p.height.is_none())
        .max_by_key(|(_, p)| p.agents.len())
        .map(|(i, _)| i)
        .unwrap_or(panels.len() - 1)
}

fn draw(f: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());

    let fill = fill_index(&app.panels);
    let constraints: Vec<Constraint> = app
        .panels
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i == fill {
                Constraint::Fill(1)
            } else {
                Constraint::Length(p.height.unwrap_or(p.content_lines() + 2).max(MIN_PANEL))
            }
        })
        .collect();
    let areas = Layout::vertical(constraints).split(body);

    for (p, area) in app.panels.iter_mut().zip(areas.iter()) {
        p.rect = *area;
        p.scroll = p.scroll.min(p.max_scroll());

        let mut counts = [0usize; 5];
        for (a, _) in &p.agents {
            counts[rank(&a.agent_status) as usize] += 1;
        }
        let title = format!(
            " {}  ▲{} ✔{} ●{} ○{} ",
            p.label, counts[0], counts[1], counts[2], counts[3]
        );
        let border = if p.error.is_some() {
            Color::Red
        } else {
            Color::Blue
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(title);

        let mut lines: Vec<Line> = Vec::new();
        if let Some(e) = &p.error {
            lines.push(Line::styled(
                format!("unreachable: {e}"),
                Style::default().fg(Color::Red),
            ));
        }
        for (a, ws) in p.sorted() {
            let (color, icon) = status_style(&a.agent_status);
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    format!("{:<9}", a.agent_status),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    a.agent.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {ws}")),
                Span::styled(
                    if p.pinned.contains(&a.pane_id) {
                        "  ★"
                    } else {
                        ""
                    },
                    Style::default().fg(Color::Yellow),
                ),
            ]));
            let detail = if a.terminal_title_stripped.is_empty() {
                short_cwd(&a.cwd)
            } else {
                format!("{}  ·  {}", short_cwd(&a.cwd), a.terminal_title_stripped)
            };
            lines.push(Line::styled(
                format!("  {detail}"),
                Style::default().fg(Color::DarkGray),
            ));
            lines.push(Line::raw(""));
        }
        if p.agents.is_empty() && p.error.is_none() {
            lines.push(Line::styled(
                "no agents",
                Style::default().fg(Color::DarkGray),
            ));
        }
        f.render_widget(
            Paragraph::new(lines).block(block).scroll((p.scroll, 0)),
            *area,
        );

        if p.max_scroll() > 0 {
            let mut sb = ScrollbarState::new(p.max_scroll() as usize).position(p.scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                area.inner(Margin::new(0, 1)),
                &mut sb,
            );
        }
    }

    if let Some(menu) = &mut app.menu {
        let items = menu.items();
        let width = items.iter().map(|i| i.len()).max().unwrap_or(0) as u16 + 4;
        let height = items.len() as u16 + 2;
        let area = f.area();
        menu.rect = Rect::new(
            menu.x.min(area.right().saturating_sub(width)),
            menu.y.min(area.bottom().saturating_sub(height)),
            width.min(area.width),
            height.min(area.height),
        );
        let lines: Vec<Line> = items
            .iter()
            .map(|i| {
                Line::styled(
                    format!(" {i} "),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            })
            .collect();
        f.render_widget(Clear, menu.rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            menu.rect,
        );
    }

    if app
        .msg
        .as_ref()
        .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4))
    {
        app.msg = None;
    }
    let footer_line = match &app.msg {
        Some((m, _)) => Line::styled(m.clone(), Style::default().fg(Color::Cyan)),
        None => Line::styled(
            "click focus · right-click menu · wheel/jk scroll · drag border resize · r reset · q quit",
            Style::default().fg(Color::DarkGray),
        ),
    };
    f.render_widget(Paragraph::new(footer_line), footer);
}

fn panel_at(app: &App, x: u16, y: u16) -> Option<usize> {
    app.panels.iter().position(|p| {
        let r = p.rect;
        x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
    })
}

/// A drag starts on the boundary between two panels (bottom border of the upper one
/// or top border of the lower one), and resizes the upper panel.
fn border_at(app: &App, x: u16, y: u16) -> Option<usize> {
    let i = panel_at(app, x, y)?;
    let r = app.panels[i].rect;
    if y == r.bottom() - 1 && i + 1 < app.panels.len() {
        Some(i)
    } else if y == r.y && i > 0 {
        Some(i - 1)
    } else {
        None
    }
}

fn resize(app: &mut App, i: usize, y: u16) {
    let top = app.panels[i].rect.y;
    let below = (app.panels.len() - 1 - i) as u16 * MIN_PANEL;
    let bottom_limit = app.panels.last().map_or(0, |p| p.rect.bottom());
    let max = bottom_limit
        .saturating_sub(top)
        .saturating_sub(below)
        .max(MIN_PANEL);
    app.panels[i].height = Some((y.saturating_sub(top) + 1).clamp(MIN_PANEL, max));
}

/// The agent card under a screen position, as (panel index, agent).
fn agent_at(app: &App, x: u16, y: u16) -> Option<(usize, Agent)> {
    let i = panel_at(app, x, y)?;
    let p = &app.panels[i];
    let inner = p.inner();
    if y < inner.y || y >= inner.bottom() {
        return None;
    }
    let line = y - inner.y + p.scroll;
    if line < p.header_lines() {
        return None;
    }
    let idx = ((line - p.header_lines()) / CARD_LINES) as usize;
    p.sorted().get(idx).map(|(agent, _)| (i, agent.clone()))
}

fn click(app: &App, x: u16, y: u16, tx: &mpsc::Sender<Reply>) {
    if let Some((i, agent)) = agent_at(app, x, y) {
        let p = &app.panels[i];
        spawn_focus(
            p.source,
            p.machine.clone(),
            p.label.clone(),
            &agent,
            tx.clone(),
        );
    }
}

fn open_menu(app: &mut App, x: u16, y: u16) {
    app.menu = agent_at(app, x, y).map(|(panel, agent)| Menu {
        panel,
        pinned: app.panels[panel].pinned.contains(&agent.pane_id),
        pane_id: agent.pane_id,
        x,
        y,
        rect: Rect::default(),
    });
}

/// Handle a left click while a menu is open. Any click closes the menu; a click
/// on an item also runs it.
fn click_menu(app: &mut App, x: u16, y: u16) {
    let Some(menu) = app.menu.take() else { return };
    let r = menu.rect;
    let inside = x > r.x && x < r.right() - 1 && y > r.y && y < r.bottom() - 1;
    if !inside {
        return;
    }
    // "Pin" / "Unpin" is the first (and so far only) item.
    let pinned = &mut app.panels[menu.panel].pinned;
    if !pinned.remove(&menu.pane_id) {
        pinned.insert(menu.pane_id);
    }
}

fn scroll(app: &mut App, delta: i32) {
    let i = app.hover.unwrap_or_else(|| fill_index(&app.panels));
    let p = &mut app.panels[i];
    p.scroll = (p.scroll as i32 + delta).clamp(0, p.max_scroll() as i32) as u16;
}

/// What survives a restart for one panel: its dragged height and its pins.
#[derive(Serialize, Deserialize, Default, PartialEq, Clone)]
struct PanelState {
    #[serde(default)]
    height: Option<u16>,
    #[serde(default)]
    pinned: Vec<String>,
}

type SavedState = BTreeMap<String, PanelState>;

fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("bistro/state.json"))
}

fn load_state() -> SavedState {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &SavedState) {
    let Some(path) = state_path() else { return };
    let Some(dir) = path.parent() else { return };
    // Best effort: losing a layout is not worth interrupting the display.
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_string_pretty(state)
        && std::fs::write(&tmp, json).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn capture_state(panels: &[Panel]) -> SavedState {
    panels
        .iter()
        .map(|p| {
            let mut pinned: Vec<String> = p.pinned.iter().cloned().collect();
            pinned.sort();
            (
                p.label.clone(),
                PanelState {
                    height: p.height,
                    pinned,
                },
            )
        })
        .collect()
}

fn restore_state(panels: &mut [Panel], saved: &SavedState) {
    for p in panels {
        if let Some(st) = saved.get(&p.label) {
            p.height = st.height;
            p.pinned = st.pinned.iter().cloned().collect();
        }
    }
}

/// Write the state to disk if it changed since the last write.
fn sync_state(app: &App, saved: &mut SavedState) {
    let now = capture_state(&app.panels);
    if now != *saved {
        save_state(&now);
        *saved = now;
    }
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        panels: vec![],
        hover: None,
        drag: None,
        msg: None,
        menu: None,
    };
    let has_orca = cli_available("orca");
    if cli_available("herdr") || !has_orca {
        app.panels.push(Panel::new("local", None));
        spawn_watcher("local".into(), tx.clone());
        for m in remote_machines() {
            spawn_poller(Source::Herdr, m.clone(), Some(m.clone()), tx.clone());
            app.panels.push(Panel::new(&m, Some(&m)));
        }
    }
    if has_orca {
        let mut panel = Panel::new("orca:local", None);
        panel.source = Source::Orca;
        spawn_poller(Source::Orca, panel.label.clone(), None, tx.clone());
        app.panels.push(panel);
    }
    let mut saved = load_state();
    restore_state(&mut app.panels, &saved);

    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        let mut dirty = last_draw.elapsed() > Duration::from_secs(1);
        sync_state(&app, &mut saved);
        while let Ok(reply) = rx.try_recv() {
            dirty = true;
            match reply {
                Reply::Ok(label, agents) => {
                    if let Some(p) = app.panels.iter_mut().find(|p| p.label == label) {
                        p.agents = agents;
                        p.error = None;
                    }
                }
                Reply::Err(label, e) => {
                    if let Some(p) = app.panels.iter_mut().find(|p| p.label == label) {
                        p.error = Some(e);
                    }
                }
                Reply::Msg(m) => app.msg = Some((m, Instant::now())),
            }
        }
        if event::poll(Duration::from_millis(200))? {
            dirty = true;
            match event::read()? {
                Event::Key(k) => match k.code {
                    KeyCode::Esc => app.menu = None,
                    KeyCode::Char('q') => {
                        sync_state(&app, &mut saved);
                        return Ok(());
                    }
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        sync_state(&app, &mut saved);
                        return Ok(());
                    }
                    KeyCode::Char('r') => app.panels.iter_mut().for_each(|p| p.height = None),
                    KeyCode::Char('j') | KeyCode::Down => scroll(&mut app, 1),
                    KeyCode::Char('k') | KeyCode::Up => scroll(&mut app, -1),
                    KeyCode::PageDown => scroll(&mut app, 10),
                    KeyCode::PageUp => scroll(&mut app, -10),
                    _ => {}
                },
                Event::Mouse(m) => {
                    if let Some(i) = panel_at(&app, m.column, m.row) {
                        app.hover = Some(i);
                    }
                    match m.kind {
                        MouseEventKind::ScrollDown => scroll(&mut app, WHEEL_STEP as i32),
                        MouseEventKind::ScrollUp => scroll(&mut app, -(WHEEL_STEP as i32)),
                        MouseEventKind::Down(MouseButton::Left) if app.menu.is_some() => {
                            click_menu(&mut app, m.column, m.row);
                        }
                        MouseEventKind::Down(MouseButton::Right) => {
                            open_menu(&mut app, m.column, m.row);
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            match border_at(&app, m.column, m.row) {
                                Some(i) => app.drag = Some(i),
                                None => click(&app, m.column, m.row, &tx),
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(i) = app.drag {
                                resize(&mut app, i, m.row);
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => app.drag = None,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if dirty {
            terminal.draw(|f| draw(f, &mut app))?;
            last_draw = Instant::now();
        }
    }
}

fn cli_available(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let res = run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

#[cfg(test)]
mod screenshots;

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    fn agent(pane: &str, status: &str) -> (Agent, String) {
        let a = Agent {
            agent: "claude".into(),
            agent_status: status.into(),
            cwd: "/tmp".into(),
            pane_id: pane.into(),
            workspace_id: "w1".into(),
            terminal_title_stripped: String::new(),
        };
        (a, "ws".into())
    }

    #[test]
    fn mixed_panels_preserve_pins_and_click_the_correct_agent() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut herdr = Panel::new("local", None);
        herdr.agents = vec![agent("term_one", "working")];
        herdr.pinned.insert("term_one".into());
        let mut orca = Panel::new("orca:local", None);
        orca.source = Source::Orca;
        orca.agents = vec![
            agent("orca:term_one", "idle"),
            agent("orca:term_two", "blocked"),
        ];
        orca.pinned.insert("orca:term_one".into());
        let mut app = App {
            panels: vec![herdr, orca],
            hover: None,
            drag: None,
            msg: None,
            menu: None,
        };
        let saved = capture_state(&app.panels);
        let mut fresh = vec![Panel::new("local", None), Panel::new("orca:local", None)];
        restore_state(&mut fresh, &saved);
        assert!(fresh[0].pinned.contains("term_one"));
        assert!(fresh[1].pinned.contains("orca:term_one"));
        assert!(!fresh[1].pinned.contains("term_one"));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let inner = app.panels[1].inner();
        let (panel, a) = agent_at(&app, inner.x, inner.y).unwrap();
        assert_eq!(panel, 1);
        assert!(app.panels[panel].source == Source::Orca);
        assert_eq!(a.pane_id, "orca:term_one");
        open_menu(&mut app, inner.x, inner.y);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let menu = app.menu.as_ref().unwrap().rect;
        click_menu(&mut app, menu.x + 1, menu.y + 1);
        assert!(!app.panels[1].pinned.contains("orca:term_one"));
        assert!(app.panels[0].pinned.contains("term_one"));
    }

    #[test]
    fn state_round_trips_through_json() {
        let mut p = Panel::new("local", None);
        p.height = Some(12);
        p.pinned.insert("w1:p2".into());
        let json = serde_json::to_string(&capture_state(&[p])).unwrap();
        let saved: SavedState = serde_json::from_str(&json).unwrap();

        let mut fresh = vec![Panel::new("local", None), Panel::new("other", None)];
        restore_state(&mut fresh, &saved);
        assert_eq!(fresh[0].height, Some(12));
        assert!(fresh[0].pinned.contains("w1:p2"));
        assert_eq!(fresh[1].height, None);
        assert!(fresh[1].pinned.is_empty());
    }

    #[test]
    fn pinned_agents_sort_above_everything() {
        let mut p = Panel::new("local", None);
        p.agents = vec![
            agent("p1", "blocked"),
            agent("p2", "idle"),
            agent("p3", "working"),
        ];
        let order = |p: &Panel| -> Vec<String> {
            p.sorted().iter().map(|(a, _)| a.pane_id.clone()).collect()
        };
        assert_eq!(order(&p), ["p1", "p3", "p2"]);
        p.pinned.insert("p2".into());
        assert_eq!(order(&p), ["p2", "p1", "p3"]);
    }

    #[test]
    fn watcher_wakes_on_event_and_subscribes_per_pane() {
        let dir = std::env::temp_dir().join(format!("bistro-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        // SAFETY: this is the only test that touches the environment.
        unsafe { std::env::set_var("HERDR_SOCKET_PATH", &path) };

        let server = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(conn.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            writeln!(
                conn,
                r#"{{"id":"bistro","result":{{"type":"subscription_started"}}}}"#
            )
            .unwrap();
            thread::sleep(Duration::from_millis(200));
            writeln!(conn, r#"{{"event":"pane.agent_status_changed"}}"#).unwrap();
            request
        });

        let mut reader = subscribe(&["w1:p1".to_string()]).expect("subscribed");
        let started = Instant::now();
        wait_for_event(&mut reader);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "woke on the event"
        );

        let request = server.join().unwrap();
        assert!(request.contains("events.subscribe"));
        assert!(request.contains(r#""pane_id":"w1:p1""#));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
