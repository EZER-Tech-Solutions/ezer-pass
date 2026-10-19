use super::app::{App, Confirm, Mode, Secret};
use crate::store;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use std::time::SystemTime;

const ACCENT: Color = Color::Green;
const DIR: Color = Color::Blue;
const MASK: &str = "••••••••••••";

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(body);

    draw_header(f, header, app);
    draw_list(f, left, app);
    draw_preview(f, right, app);
    draw_footer(f, footer, app);

    match &app.mode {
        Mode::Search(_) => draw_search(f, app),
        Mode::Confirm(confirm) => draw_confirm(f, confirm, app.upstream.as_deref()),
        Mode::Help => draw_help(f),
        Mode::Browse | Mode::Command(_) => {}
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let left = Line::from(vec![
        " ezer-pass ".fg(Color::Black).bg(ACCENT).bold(),
        " ".into(),
        store::display(&app.sess.cwd).fg(DIR).bold(),
    ]);
    f.render_widget(Paragraph::new(left), area);

    if let Some(upstream) = &app.upstream {
        let sync = if app.ahead > 0 {
            format!("{upstream} ↑{} ", app.ahead).yellow()
        } else {
            format!("{upstream} ✓ ").dim()
        };
        f.render_widget(Paragraph::new(Line::from(sync)).right_aligned(), area);
    }
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::bordered()
        .title(" entries ")
        .border_style(Style::new().dim());
    if app.nodes.is_empty() {
        f.render_widget(Paragraph::new("(empty)".dim()).block(block), area);
        return;
    }
    let items: Vec<ListItem> = app
        .nodes
        .iter()
        .map(|n| {
            if n.is_dir {
                ListItem::new(format!("{}/", n.name).fg(DIR).bold())
            } else {
                ListItem::new(n.name.as_str())
            }
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut app.list);
}

fn draw_preview(f: &mut Frame, area: Rect, app: &App) {
    let Some(node) = app.selected() else {
        f.render_widget(Block::bordered().border_style(Style::new().dim()), area);
        return;
    };
    let path = app.selected_path().unwrap_or_default();
    let title = if node.is_dir {
        format!(" {}/ ", store::display(&path))
    } else {
        format!(" {} ", store::display(&path))
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().dim());

    let lines: Vec<Line> = if node.is_dir {
        match app.sess.store.list(&path) {
            Ok(children) if children.is_empty() => vec!["(empty directory)".dim().into()],
            Ok(children) => children
                .into_iter()
                .map(|c| {
                    if c.is_dir {
                        format!("{}/", c.name).fg(DIR).into()
                    } else {
                        c.name.into()
                    }
                })
                .collect(),
            Err(e) => vec![format!("{e:#}").red().into()],
        }
    } else if let Some(d) = app.current_secret() {
        match &d.secret {
            Secret::Text(text) => secret_lines(text, d.revealed_at.is_some()),
            Secret::File { filename, size } => vec![
                Line::from(vec![
                    "file entry ".yellow().bold(),
                    filename.as_str().into(),
                ]),
                format!("{size} bytes").dim().into(),
                "".into(),
                hint(&[("x", "export to disk")]),
            ],
        }
    } else {
        let mut lines = vec!["encrypted".dim().into()];
        if let Some(t) = app.sess.store.modified(&path) {
            lines.push(format!("modified {}", ago(t)).dim().into());
        }
        lines.push("".into());
        lines.push(hint(&[
            ("⏎", "decrypt"),
            ("space", "reveal"),
            ("c", "copy"),
        ]));
        lines
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The first line of a text entry is the password: masked unless revealed.
/// Later lines (user:, url:, notes) are shown, with `key:` prefixes tinted.
fn secret_lines(text: &str, revealed: bool) -> Vec<Line<'_>> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut out = vec![if revealed {
        first.yellow().bold().into()
    } else {
        Line::from(vec![MASK.dim(), "  space to reveal".dim().italic()])
    }];
    for line in lines {
        out.push(match line.split_once(": ") {
            Some((k, v)) if !k.is_empty() && !k.contains(' ') => {
                Line::from(vec![format!("{k}: ").cyan(), v.into()])
            }
            _ => line.into(),
        });
    }
    out
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if let Mode::Command(input) = &app.mode {
        f.render_widget(Paragraph::new(format!(":{}", input.text)), area);
        let x = area.x + 1 + input.cursor_col() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        return;
    }
    let line = match &app.status {
        Some(s) if s.error => Line::from(s.text.as_str().red()),
        Some(s) => Line::from(s.text.as_str().fg(ACCENT)),
        None => hint(&[
            ("jk", "move"),
            ("hl", "dirs"),
            ("⏎", "open"),
            ("space", "reveal"),
            ("c", "copy"),
            ("/", "search"),
            (":", "command"),
            ("?", "help"),
            ("q", "quit"),
        ]),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn hint(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (k, desc)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push("  ".into());
        }
        spans.push(k.to_string().fg(ACCENT).bold());
        spans.push(format!(" {desc}").dim());
    }
    Line::from(spans)
}

fn draw_search(f: &mut Frame, app: &mut App) {
    let Mode::Search(search) = &mut app.mode else {
        return;
    };
    let area = popup(f.area(), 70, 70);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(
            " search {}/{} ",
            search.results.len(),
            search.total()
        ))
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            "/ ".fg(ACCENT).bold(),
            search.input.text.as_str().into(),
        ])),
        input_area,
    );
    f.set_cursor_position((
        input_area.x + 2 + search.input.cursor_col() as u16,
        input_area.y,
    ));

    let items: Vec<ListItem> = search
        .results
        .iter()
        .map(|(path, is_dir)| {
            if *is_dir {
                ListItem::new(format!("{}/", store::display(path)).fg(DIR))
            } else {
                ListItem::new(store::display(path))
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, list_area, &mut search.list);
}

fn draw_confirm(f: &mut Frame, confirm: &Confirm, upstream: Option<&str>) {
    let (question, default_yes) = match confirm {
        Confirm::Delete { path, is_dir } => (
            format!(
                "delete {}{}{}?",
                if *is_dir { "directory " } else { "" },
                store::display(path),
                if *is_dir { " and everything in it" } else { "" }
            ),
            false,
        ),
        Confirm::Pull => (
            format!("pull from {}?", upstream.unwrap_or("upstream")),
            true,
        ),
        Confirm::Push { ahead } => (
            format!(
                "push {ahead} commit{} to {}?",
                if *ahead == 1 { "" } else { "s" },
                upstream.unwrap_or("upstream")
            ),
            true,
        ),
    };
    let choices = if default_yes { "[Y/n]" } else { "[y/N]" };
    let width = (question.chars().count() + 10).clamp(30, f.area().width as usize) as u16;
    let area = popup_fixed(f.area(), width, 5);
    f.render_widget(Clear, area);
    let color = if default_yes { ACCENT } else { Color::Red };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(question),
            "".into(),
            Line::from(choices.fg(color).bold()),
        ])
        .centered()
        .wrap(Wrap { trim: true })
        .block(Block::bordered().border_style(Style::new().fg(color))),
        area,
    );
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("j k ↑ ↓", "move (PgUp/PgDn/Home/End)"),
        ("l → ⏎", "open directory / decrypt entry"),
        ("h ← ⌫", "parent directory"),
        ("space", "reveal / hide password"),
        ("c", "copy password to clipboard"),
        ("/", "search the whole store"),
        ("e", "edit entry in $EDITOR"),
        ("g", "generate a password  (:generate)"),
        ("i", "insert an entry      (:insert)"),
        ("m", "make a directory     (:mkdir)"),
        ("r", "rename / move        (:mv)"),
        ("d", "delete"),
        ("I", "import a file        (:import)"),
        ("x", "export a file entry  (:export)"),
        (":", "run any command (:git log, :find, :cp ...)"),
        ("R", "refresh"),
        ("q", "quit (offers to push)"),
    ];
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, desc)| Line::from(vec![format!(" {k:<9}").fg(ACCENT).bold(), (*desc).into()]))
        .collect();
    lines.push("".into());
    lines.push(" any key to close".dim().into());
    let area = popup_fixed(f.area(), 58, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" keys ")
                .border_style(Style::new().fg(ACCENT)),
        ),
        area,
    );
}

/// A centered rect taking the given percentage of `area`.
fn popup(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Percentage(pct_y)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Percentage(pct_x)])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// A centered rect of fixed size, clamped to `area`.
fn popup_fixed(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// "3 days ago"-style age of a timestamp.
fn ago(t: SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86400 => (s / 3600, "hour"),
        s if s < 86400 * 60 => (s / 86400, "day"),
        s if s < 86400 * 730 => (s / (86400 * 30), "month"),
        s => (s / (86400 * 365), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}
