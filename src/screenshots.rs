//! Makes the README screenshots from made-up data, so no real session is shown.
//!
//!     cargo test screenshots -- --ignored
//!
//! Writes `docs/screenshots/bistro-{1,2,3}-hosts.svg`. Convert to PNG with
//! `rsvg-convert -z 1 in.svg -o out.png`. Only the PNGs are committed.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;

const COLS: u16 = 58;
const ROWS: u16 = 42;
const CELL_W: f32 = 9.6;
const CELL_H: f32 = 20.0;

fn agent(
    pane: &str,
    kind: &str,
    status: &str,
    cwd: &str,
    title: &str,
    ws: &str,
) -> (Agent, String) {
    let a = Agent {
        agent: kind.into(),
        agent_status: status.into(),
        cwd: cwd.into(),
        pane_id: pane.into(),
        workspace_id: ws.into(),
        terminal_title_stripped: title.into(),
    };
    (a, ws.into())
}

fn hosts() -> Vec<Panel> {
    let mut local = Panel::new("studio", None);
    local.agents = vec![
        agent(
            "w1:p1",
            "claude",
            "blocked",
            "/work/storefront",
            "approve: run migrations?",
            "storefront",
        ),
        agent(
            "w1:p2",
            "codex",
            "working",
            "/work/storefront",
            "writing checkout tests",
            "storefront",
        ),
        agent(
            "w2:p1",
            "pi",
            "done",
            "/work/docs-site",
            "link checker fixed",
            "docs-site",
        ),
        agent(
            "w3:p1",
            "claude",
            "working",
            "/work/api-gateway",
            "refactoring rate limiter",
            "api-gateway",
        ),
        agent(
            "w3:p2",
            "claude",
            "idle",
            "/work/api-gateway",
            "",
            "api-gateway",
        ),
        agent(
            "w4:p1",
            "opencode",
            "idle",
            "/work/dotfiles",
            "",
            "dotfiles",
        ),
    ];
    local.pinned.insert("w3:p1".into());
    local.pinned.insert("w1:p1".into());

    let mut build = Panel::new("build-box", Some("build-box"));
    build.agents = vec![
        agent(
            "w1:p1",
            "claude",
            "working",
            "/srv/ci/runner",
            "bisecting flaky test",
            "ci-runner",
        ),
        agent(
            "w2:p1",
            "codex",
            "done",
            "/srv/ci/images",
            "base image rebuilt",
            "images",
        ),
        agent(
            "w2:p2",
            "pi",
            "blocked",
            "/srv/ci/images",
            "needs registry token",
            "images",
        ),
    ];
    build.pinned.insert("w2:p2".into());

    let mut mini = Panel::new("mac-mini", Some("mac-mini"));
    mini.agents = vec![
        agent(
            "w1:p1",
            "claude",
            "idle",
            "/Users/dev/ios-app",
            "",
            "ios-app",
        ),
        agent(
            "w1:p2",
            "codex",
            "working",
            "/Users/dev/ios-app",
            "updating snapshot tests",
            "ios-app",
        ),
    ];

    vec![local, build, mini]
}

fn color(c: Color, fg: bool) -> &'static str {
    match c {
        Color::Reset => {
            if fg {
                "#cdd6f4"
            } else {
                "#1e1e2e"
            }
        }
        Color::Black => "#45475a",
        Color::Red => "#f38ba8",
        Color::Green => "#a6e3a1",
        Color::Yellow => "#f9e2af",
        Color::Blue => "#89b4fa",
        Color::Magenta => "#f5c2e7",
        Color::Cyan => "#94e2d5",
        Color::Gray => "#a6adc8",
        Color::DarkGray => "#7f849c",
        _ => "#cdd6f4",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn to_svg(buf: &Buffer) -> String {
    let (w, h) = (
        buf.area.width as f32 * CELL_W,
        buf.area.height as f32 * CELL_H,
    );
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#1e1e2e\"/>\n\
         <g font-family=\"JetBrainsMono NF, JetBrains Mono, monospace\" font-size=\"16\">\n"
    );
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let (px, py) = (x as f32 * CELL_W, y as f32 * CELL_H);
            if cell.bg != Color::Reset {
                out += &format!(
                    "<rect x=\"{px}\" y=\"{py}\" width=\"{CELL_W}\" height=\"{CELL_H}\" fill=\"{}\"/>\n",
                    color(cell.bg, false)
                );
            }
            let sym = cell.symbol();
            if sym.trim().is_empty() {
                continue;
            }
            let bold = cell.modifier.contains(Modifier::BOLD);
            out += &format!(
                "<text x=\"{px}\" y=\"{}\" fill=\"{}\"{}>{}</text>\n",
                py + CELL_H - 5.0,
                color(cell.fg, true),
                // The main font lacks the check mark and a colour-emoji font would win the fallback.
                if sym == "✔" {
                    " font-family=\"Adwaita Mono\""
                } else if bold {
                    " font-weight=\"bold\""
                } else {
                    ""
                },
                esc(sym)
            );
        }
    }
    out + "</g>\n</svg>\n"
}

fn render(n: usize, menu: bool) -> String {
    let mut app = App {
        panels: hosts().into_iter().take(n).collect(),
        hover: None,
        drag: None,
        msg: None,
        menu: None,
    };
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    if menu {
        // Open the menu on the third card of the first panel (not pinned, so it offers Pin), as a right click would.
        let r = app.panels[0].inner();
        let (x, y) = (r.x + 34, r.y + 2 * CARD_LINES);
        open_menu(&mut app, x, y);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
    }
    to_svg(terminal.backend().buffer())
}

#[test]
#[ignore = "writes files; run on demand"]
fn screenshots() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        let svg = render(n, n == 2);
        std::fs::write(dir.join(format!("bistro-{n}-hosts.svg")), svg).unwrap();
    }
}
