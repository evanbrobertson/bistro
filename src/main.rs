use std::collections::HashMap;
use std::io::stdout;
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
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{DefaultTerminal, Frame};
use serde::Deserialize;

const POLL: Duration = Duration::from_secs(2);
const CARD_LINES: u16 = 3;
const MIN_PANEL: u16 = 3;
const WHEEL_STEP: u16 = 3;

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
    /// `--machine` argument; `None` for the local session.
    machine: Option<String>,
    agents: Vec<(Agent, String)>,
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
            machine: machine.map(String::from),
            agents: vec![],
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
        v.sort_by_key(|(a, _)| rank(&a.agent_status));
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

fn spawn_poller(label: String, machine: Option<String>, tx: mpsc::Sender<Reply>) {
    thread::spawn(move || {
        loop {
            let reply = match fetch(machine.as_deref()) {
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

fn spawn_focus(machine: Option<String>, label: String, agent: &Agent, tx: mpsc::Sender<Reply>) {
    let pane = agent.pane_id.clone();
    let kind = agent.agent.clone();
    thread::spawn(move || {
        let msg = match herdr(machine.as_deref(), &["agent", "focus", &pane]) {
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
            "click focus · wheel/jk scroll · drag border resize · r reset · q quit",
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

fn click(app: &App, x: u16, y: u16, tx: &mpsc::Sender<Reply>) {
    let Some(i) = panel_at(app, x, y) else { return };
    let p = &app.panels[i];
    let inner = p.inner();
    if y < inner.y || y >= inner.bottom() {
        return;
    }
    let line = y - inner.y + p.scroll;
    if line < p.header_lines() {
        return;
    }
    let idx = ((line - p.header_lines()) / CARD_LINES) as usize;
    if let Some((agent, _)) = p.sorted().get(idx) {
        spawn_focus(p.machine.clone(), p.label.clone(), agent, tx.clone());
    }
}

fn scroll(app: &mut App, delta: i32) {
    let i = app.hover.unwrap_or_else(|| fill_index(&app.panels));
    let p = &mut app.panels[i];
    p.scroll = (p.scroll as i32 + delta).clamp(0, p.max_scroll() as i32) as u16;
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        panels: vec![Panel::new("local", None)],
        hover: None,
        drag: None,
        msg: None,
    };
    spawn_poller("local".into(), None, tx.clone());
    for m in remote_machines() {
        spawn_poller(m.clone(), Some(m.clone()), tx.clone());
        app.panels.push(Panel::new(&m, Some(&m)));
    }

    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        let mut dirty = last_draw.elapsed() > Duration::from_secs(1);
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
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
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

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let res = run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}
