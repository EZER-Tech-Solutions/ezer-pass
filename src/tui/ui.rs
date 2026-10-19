use super::app::{App, Confirm, Mode, Secret, basename};
use crate::{index, store};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use std::time::SystemTime;

const ACCENT: Color = Color::Green;
const DIR: Color = Color::Blue;
const MARK: Color = Color::Yellow;
/// File entries (base64-stored files), told apart via the file index.
const FILE: Color = Color::Magenta;
const MASK: &str = "••••••••••••";
/// Width of the highlight bar the list draws before every row.
const BAR: &str = "▌";

pub fn draw(f: &mut Frame, app: &mut App) {
    let [tree, bottom, footer] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [recent, info] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(bottom);

    draw_tree(f, tree, app);
    draw_recent(f, recent, app);
    draw_info(f, info, app);
    draw_footer(f, footer, app);

    match &app.mode {
        Mode::Confirm(confirm) => draw_confirm(f, confirm, app.upstream.as_deref()),
        Mode::View(_) => draw_view(f, app),
        Mode::Help => draw_help(f),
        _ => {}
    }
}

fn pane(title: &str) -> Block<'_> {
    Block::bordered()
        .title(Line::from(format!(" {title} ")).bold())
        .border_style(Style::new().dim())
}

fn draw_tree(f: &mut Frame, area: Rect, app: &mut App) {
    let root = short_root(app, area.width.saturating_sub(16) as usize);
    let block = Block::bordered()
        .title(Line::from(vec![
            " ezer-pass ".fg(Color::Black).bg(ACCENT).bold(),
            " ".into(),
        ]))
        .title_top(Line::from(format!(" {root} ").dim()).right_aligned())
        .border_style(Style::new().dim());
    if app.rows.is_empty() {
        let msg = if app.filtering() {
            "no matches"
        } else {
            "(empty store)"
        };
        f.render_widget(Paragraph::new(msg.dim()).block(block), area);
        return;
    }

    let cut: Vec<&str> = match &app.buffer {
        Some(b) if b.cut => b.paths.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let copied: Vec<&str> = match &app.buffer {
        Some(b) if !b.cut => b.paths.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let renaming = match &app.mode {
        Mode::Rename { path, input } => Some((path.as_str(), input)),
        _ => None,
    };

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let flag = if app.marks.contains(&row.path) {
                "● ".fg(MARK).bold()
            } else if cut.contains(&row.path.as_str()) {
                "✂ ".fg(MARK)
            } else if copied.contains(&row.path.as_str()) {
                "+ ".fg(MARK)
            } else {
                "  ".into()
            };
            let is_file = !row.is_dir && app.sess.files.is_file(&row.path);
            // Every icon is two columns wide; the rename cursor relies on it.
            let icon = match (row.is_dir, app.is_expanded(&row.path)) {
                (true, true) => "▾ ".fg(DIR),
                (true, false) => "▸ ".fg(DIR),
                (false, _) if is_file => "▪ ".fg(FILE),
                (false, _) => "  ".into(),
            };
            let mut spans = vec![flag, Span::raw("  ".repeat(row.depth)), icon];
            match renaming {
                Some((path, input)) if path == row.path => {
                    spans.push(input.text.clone().underlined());
                }
                _ => {
                    let mut name = if row.is_dir {
                        format!("{}/", row.name).fg(DIR).bold()
                    } else if is_file {
                        row.name.clone().fg(FILE)
                    } else {
                        row.name.clone().into()
                    };
                    if row.context_only || cut.contains(&row.path.as_str()) {
                        name = name.dim();
                    }
                    spans.push(name);
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(BAR);
    f.render_stateful_widget(list, area, &mut app.list);

    // Put the terminal cursor in the row being renamed.
    if let (Some((_, input)), Some(row), Some(i)) = (renaming, app.selected(), app.list.selected())
    {
        let x = area.x + 1 + BAR.chars().count() as u16 + 2 + 2 * row.depth as u16 + 2;
        let y = area.y + 1 + (i - app.list.offset()) as u16;
        f.set_cursor_position((
            (x + input.cursor_col() as u16).min(area.right().saturating_sub(2)),
            y,
        ));
    }
}

fn draw_recent(f: &mut Frame, area: Rect, app: &App) {
    let clip = app.clip_remaining();
    let mut lines: Vec<Line> = app
        .recent
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let shown = store::display(path);
            let mut spans = vec![
                format!(" {} ", i + 1).fg(ACCENT).bold(),
                if app.sess.files.is_file(path) {
                    shown.fg(FILE)
                } else {
                    shown.into()
                },
            ];
            if let Some((p, secs)) = clip
                && p == path
            {
                spans.push(format!("  ⧉ {secs}s").fg(MARK));
            }
            Line::from(spans)
        })
        .collect();
    if lines.is_empty() {
        lines.push(" copy with ⏎ or c; recent ones land here".dim().into());
    }
    f.render_widget(Paragraph::new(lines).block(pane("recent")), area);
}

fn draw_info(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    match app.selected() {
        Some(row) if row.is_dir => {
            lines.push(
                format!("{}/", store::display(&row.path))
                    .fg(DIR)
                    .bold()
                    .into(),
            );
            let detail = match app.sess.store.list(&row.path) {
                Ok(children) => {
                    let count = |pred: &dyn Fn(&store::Node) -> bool| {
                        children.iter().filter(|c| pred(c)).count()
                    };
                    let dirs = count(&|c| c.is_dir);
                    let files = count(&|c| {
                        !c.is_dir && app.sess.files.is_file(&format!("{}/{}", row.path, c.name))
                    });
                    let passwords = children.len() - dirs - files;
                    let plural = |n: usize, one: &str, many: &str| {
                        format!("{n} {}", if n == 1 { one } else { many })
                    };
                    let mut parts = vec![
                        "folder".to_string(),
                        plural(passwords, "password", "passwords"),
                    ];
                    if files > 0 {
                        parts.push(plural(files, "file", "files"));
                    }
                    parts.push(plural(dirs, "folder", "folders"));
                    parts.join(" · ")
                }
                Err(_) => "folder".into(),
            };
            lines.push(detail.dim().into());
        }
        Some(row) => {
            let size = app.sess.files.size(&row.path);
            let mut detail = match size {
                Some(bytes) => format!("file · {}", index::human_size(bytes)),
                None => "password".to_string(),
            };
            if let Some(t) = app.sess.store.modified(&row.path) {
                detail.push_str(&format!(" · modified {}", ago(t)));
            }
            if size.is_some() {
                lines.push(Line::from(vec![
                    store::display(&row.path).fg(FILE).bold(),
                    "  ⏎ export · v view".dim(),
                ]));
            } else {
                lines.push(store::display(&row.path).bold().into());
            }
            lines.push(detail.dim().into());
        }
        None => {
            lines.push("nothing selected".dim().into());
            lines.push("".into());
        }
    }

    lines.push(match &app.buffer {
        Some(b) => {
            let names: Vec<&str> = b.paths.iter().map(|p| basename(p)).collect();
            Line::from(vec![
                if b.cut { "✂ cut " } else { "+ copied " }.fg(MARK).bold(),
                names.join(", ").into(),
                "  p paste".dim(),
            ])
        }
        None => "buffer empty · x cut · y copy".dim().into(),
    });

    lines.push(if app.marks.is_empty() {
        "".into()
    } else {
        Line::from(vec![
            format!("● {} marked", app.marks.len()).fg(MARK).bold(),
            "  x cut · y copy · d delete · esc clear".dim(),
        ])
    });

    lines.push(match &app.upstream {
        Some(up) if app.ahead > 0 => Line::from(vec![
            "git ".dim(),
            up.as_str().into(),
            format!(" ↑{} unpushed", app.ahead).yellow(),
        ]),
        Some(up) => Line::from(vec!["git ".dim(), up.as_str().into(), " ✓ in sync".dim()]),
        None => "git: no upstream".dim().into(),
    });

    f.render_widget(Paragraph::new(lines).block(pane("info")), area);
}

/// The first line of a text entry is the password: masked unless revealed.
/// Later lines (user:, url:, notes) are shown, with `key:` prefixes tinted.
fn secret_lines(text: &str, revealed: bool) -> Vec<Line<'_>> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut out = vec![if revealed {
        first.yellow().bold().into()
    } else {
        MASK.dim().into()
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

fn draw_view(f: &mut Frame, app: &App) {
    let Mode::View(view) = &app.mode else {
        return;
    };
    let mut lines = match &view.secret {
        Secret::Text(text) => secret_lines(text, view.revealed_at.is_some()),
        Secret::File { filename, size } => vec![
            Line::from(vec![
                "file entry ".yellow().bold(),
                filename.as_str().into(),
            ]),
            format!("{size} bytes").dim().into(),
        ],
    };
    lines.push("".into());
    lines.push(match &view.secret {
        Secret::Text(_) => hint(&[("space", "reveal"), ("c", "copy"), ("esc", "close")]),
        Secret::File { .. } => hint(&[("E", "export"), ("esc", "close")]),
    });
    let width = (f.area().width * 6 / 10).max(40);
    let area = popup_fixed(f.area(), width, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {} ", store::display(&view.path)))
                .border_style(Style::new().fg(ACCENT)),
        ),
        area,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let input = match &app.mode {
        Mode::Command(input) => {
            let prefix = format!("{}/ ", store::display(&app.sess.cwd).trim_end_matches('/'));
            Some((vec![prefix.dim(), ":".fg(ACCENT).bold()], input))
        }
        Mode::Filter => app
            .filter
            .as_ref()
            .map(|flt| (vec!["/".fg(ACCENT).bold()], &flt.input)),
        _ => None,
    };
    if let Some((mut spans, input)) = input {
        let offset: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(input.text.as_str().into());
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        let x = area.x + (offset + input.cursor_col()) as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        return;
    }
    let line = match &app.status {
        Some(s) if s.error => Line::from(s.text.as_str().red()),
        Some(s) => Line::from(s.text.as_str().fg(ACCENT)),
        None if app.filtering() => Line::from(vec![
            "/".fg(ACCENT).bold(),
            app.filter
                .as_ref()
                .map_or("", |f| f.input.text.as_str())
                .into(),
            "   esc clear · / edit".dim(),
        ]),
        None => hint(&[
            ("⏎", "copy/open"),
            ("v", "view"),
            ("space", "mark"),
            ("x/y/p", "cut/copy/paste"),
            ("r", "rename"),
            ("/", "filter"),
            ("1-5", "recent"),
            ("?", "help"),
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

fn draw_confirm(f: &mut Frame, confirm: &Confirm, upstream: Option<&str>) {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let (question, default_yes) = match confirm {
        Confirm::Delete(targets) => (
            match targets.as_slice() {
                [(path, true)] => format!(
                    "delete folder {}/ and everything in it?",
                    store::display(path)
                ),
                [(path, false)] => format!("delete {}?", store::display(path)),
                _ => format!(
                    "delete {} items{}?",
                    targets.len(),
                    if targets.iter().any(|t| t.1) {
                        " (folders with everything in them)"
                    } else {
                        ""
                    }
                ),
            },
            false,
        ),
        Confirm::Overwrite { conflicts, .. } => (
            format!(
                "{conflicts} entr{} already exist{} there — overwrite?",
                if *conflicts == 1 { "y" } else { "ies" },
                if *conflicts == 1 { "s" } else { "" }
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
                plural(*ahead as usize),
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
        ("l →", "expand folder / step into it"),
        ("h ←", "collapse folder / jump to parent"),
        ("H ⌫", "collapse the folder you're in"),
        ("⏎", "copy password · export file · toggle folder"),
        ("-", "collapse everything"),
        ("v", "view entry (space reveals)"),
        ("c", "copy password to clipboard"),
        ("1-5", "copy a recent entry again"),
        ("/", "filter the tree (fuzzy)"),
        ("space", "mark / unmark"),
        ("x  y", "cut / copy marked (or highlighted)"),
        ("p", "paste into highlighted folder"),
        ("r", "rename in place (../ and /abs work)"),
        ("d", "delete marked (or highlighted)"),
        ("e", "edit entry in $EDITOR"),
        ("g i m", "generate / insert / mkdir here"),
        ("I  E", "import a file here / export entry"),
        (":", "run any command (:git log, :find ...)"),
        ("esc", "clear filter, then marks, then buffer"),
        ("R  q", "refresh / quit (offers to push)"),
    ];
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, desc)| Line::from(vec![format!(" {k:<9}").fg(ACCENT).bold(), (*desc).into()]))
        .collect();
    lines.push("".into());
    lines.push(" any key to close".dim().into());
    let area = popup_fixed(f.area(), 54, lines.len() as u16 + 2);
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

/// The store root for the tree's title: `~` for $HOME, and cut from the
/// left to fit `max` columns.
fn short_root(app: &App, max: usize) -> String {
    let mut root = app.sess.store.root().display().to_string();
    if let Some(home) = std::env::var_os("HOME")
        && let Some(rest) = root.strip_prefix(&*home.to_string_lossy())
    {
        root = format!("~{rest}");
    }
    let len = root.chars().count();
    if len > max {
        root = format!(
            "…{}",
            root.chars().skip(len + 1 - max.max(1)).collect::<String>()
        );
    }
    root
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
