use super::app::{App, Confirm, Insert, Mode, Picker, Row, Secret, Unlock, basename, join};
use super::generate::{Field, Form};
use super::history;
use super::palette;
use super::preview::{Body, Preview};
use super::sync::{Op, Sync};
use crate::password::{Kind, LOOK_ALIKES, SEPARATORS, separator_name};
use crate::{index, store};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::scrollbar;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use std::sync::OnceLock;
use std::time::SystemTime;

// Picked by eye on a dark theme. 0-15 follow the terminal's theme;
// indexed colors above that are fixed.
const ACCENT: Color = Color::Indexed(214);
const DIR: Color = Color::Cyan;
const MARK: Color = Color::LightMagenta;
/// File entries (base64-stored files), told apart via the file index.
const FILE: Color = Color::Magenta;
const DANGER: Color = Color::Red;
/// The passphrase and insert popups, so they stand apart from the rest.
const UNLOCK: Color = Color::LightRed;
const MASK: &str = "••••••••••••";
/// Width of the highlight bar the list draws before every row.
const BAR: &str = "▌";
/// The dark grey behind the title chips and the highlighted row.
const SHADE: Color = Color::Indexed(236);
/// Behind the popup input fields; darker than `SHADE` so it stays quiet.
const FIELD: Color = Color::Indexed(234);
/// Pane borders.
const BORDER: Color = Color::Indexed(242);
/// Width of the label column in the info pane.
const LABEL: usize = 8;
/// The narrowest terminal that gets the preview pane beside the tree.
const PREVIEW_MIN_WIDTH: u16 = 110;

/// Whether a terminal `width` columns wide shows the preview pane.
pub fn preview_fits(width: u16) -> bool {
    width >= PREVIEW_MIN_WIDTH
}

pub fn draw(f: &mut Frame, app: &mut App) {
    if let Mode::History(_) = app.mode {
        return draw_history(f, app);
    }
    let [tree, bottom, footer] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(7),
        Constraint::Length(footer_rows(app, f.area().width)),
    ])
    .areas(f.area());
    let [recent, info] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(bottom);

    if let Some(preview) = &app.preview {
        let side = (tree.width * 4 / 10).clamp(40, 60);
        let [tree, side] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(side)]).areas(tree);
        draw_preview(f, side, app, preview);
        draw_tree(f, tree, app);
    } else {
        draw_tree(f, tree, app);
    }
    draw_recent(f, recent, app);
    draw_info(f, info, app);
    draw_footer(f, footer, app);

    if matches!(
        app.mode,
        Mode::Confirm(_)
            | Mode::View(_)
            | Mode::Help
            | Mode::Sync(_)
            | Mode::Generate(_)
            | Mode::Unlock(_)
            | Mode::Insert(_)
            | Mode::Contexts(_)
    ) {
        // Push everything behind a popup into the background.
        let area = f.area();
        f.buffer_mut().set_style(area, Style::new().dim());
    }
    match &app.mode {
        Mode::Confirm(confirm) => draw_confirm(f, app, confirm),
        Mode::View(_) => draw_view(f, app),
        Mode::Help => draw_help(f),
        Mode::Generate(form) => draw_generate(f, form),
        Mode::Command(input) => {
            let palette = palette::suggest(&input.text, &app.sess.store, &app.sess.cwd);
            draw_palette(f, footer, &palette, app.palette_sel);
        }
        Mode::Sync(sync) => draw_sync(f, app, sync),
        Mode::Unlock(u) => draw_unlock(f, u),
        Mode::Insert(ins) => draw_insert(f, ins),
        Mode::Contexts(picker) => draw_contexts(f, app, picker),
        _ => {}
    }
}

/// The passphrase popup: one masked field, then the held action runs.
fn draw_unlock(f: &mut Frame, u: &Unlock) {
    let len = u.input.text.chars().count();
    let status = match (&u.error, u.working) {
        (_, true) => Line::from("unlocking…".fg(UNLOCK)),
        (Some(e), _) => Line::from(vec!["✗ ".fg(DANGER).bold(), e.clone().fg(DANGER)]),
        (None, _) => "once per session; kept in memory only".dim().into(),
    };
    let width = 56.min(f.area().width);
    let (prompt, col) = input_line("passphrase: ", len, len, '\u{f023}', width);
    let lines = vec![prompt, "".into(), status];
    let mut keys = hint_in(&[("⏎", "unlock"), ("esc", "cancel")], UNLOCK);
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let block = popup(Line::from(" unlock ").bold(), UNLOCK)
        .title_bottom(keys.right_aligned())
        .padding(Padding::new(1, 2, 1, 1));
    let area = popup_fixed(f.area(), width, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(Paragraph::new(lines).block(block), area);
    if !u.working {
        f.set_cursor_position((inner.x + col, inner.y));
    }
}

/// The insert popup: the new entry's value, masked like the passphrase.
fn draw_insert(f: &mut Frame, ins: &Insert) {
    let shown = store::display(&ins.path);
    let status = if ins.exists {
        Line::from(format!("{shown} exists — ⏎ replaces it").fg(MARK))
    } else {
        "encrypted into the store; never shown".dim().into()
    };
    let width = 56.min(f.area().width);
    let len = ins.input.text.chars().count();
    let (prompt, col) = input_line("value: ", len, ins.input.cursor_col(), '\u{f084}', width);
    let lines = vec![prompt, "".into(), status];
    let mut keys = hint_in(&[("⏎", "save"), ("esc", "cancel")], UNLOCK);
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let block = popup(Line::from(format!(" insert {shown} ")).bold(), UNLOCK)
        .title_bottom(keys.right_aligned())
        .padding(Padding::new(1, 2, 1, 1));
    let area = popup_fixed(f.area(), width, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(Paragraph::new(lines).block(block), area);
    f.set_cursor_position((inner.x + col, inner.y));
}

fn pane(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(format!(" {title} ")).bold())
        .border_style(Style::new().fg(BORDER))
}

fn popup(title: Line<'static>, color: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(Style::new().fg(color))
        .padding(Padding::horizontal(1))
}

/// How an entry's name is drawn: file entries in their own color.
fn entry_style(app: &App, path: &str) -> Style {
    if app.sess.files.is_file(path) {
        Style::new().fg(FILE)
    } else {
        Style::new()
    }
}

/// A store path as a dim parent followed by the name in `style`.
fn path_spans(path: &str, style: Style) -> Vec<Span<'static>> {
    let shown = store::display(path);
    let split = shown.rfind('/').map_or(0, |i| i + 1);
    vec![
        shown[..split].to_string().dim(),
        Span::styled(shown[split..].to_string(), style),
    ]
}

/// A folder's icon, with the space after it: Nerd Font folder glyphs, or
/// triangles with `EZER_PASS_ICONS=plain` for fonts without them.
fn folder_icon(open: bool) -> &'static str {
    static PLAIN: OnceLock<bool> = OnceLock::new();
    let plain = *PLAIN.get_or_init(|| std::env::var("EZER_PASS_ICONS").is_ok_and(|v| v == "plain"));
    match (plain, open) {
        (false, true) => "\u{f07c} ",
        (false, false) => "\u{f07b} ",
        (true, true) => "▼ ",
        (true, false) => "▶ ",
    }
}

/// Tree guides for each row: `│ ` under ancestors with siblings further
/// down, then `├─`/`└─` joining the row to its parent. Two columns per level
/// below the top, which the rename cursor relies on.
fn guides(rows: &[Row]) -> Vec<String> {
    let mut out = vec![String::new(); rows.len()];
    // more[d]: a later row at depth d shares the current row's ancestry.
    let mut more: Vec<bool> = Vec::new();
    for (i, row) in rows.iter().enumerate().rev() {
        let d = row.depth;
        more.resize(d + 1, false);
        let mut g: String = (1..d).map(|k| if more[k] { "│ " } else { "  " }).collect();
        if d > 0 {
            g.push_str(if more[d] { "├─" } else { "└─" });
        }
        out[i] = g;
        more[d] = true;
    }
    out
}

fn draw_tree(f: &mut Frame, area: Rect, app: &mut App) {
    let mut title = vec![" ezer-pass ".fg(Color::LightMagenta).bg(SHADE).bold()];
    if app.show_context {
        title.push(" ".into());
        title.push(
            format!(" {} ", app.sess.context.name)
                .fg(ACCENT)
                .bg(SHADE)
                .bold(),
        );
    }
    title.push(" ".into());
    let title = Line::from(title);
    let root = short_root(app, (area.width as usize).saturating_sub(title.width() + 5));
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .title_top(Line::from(format!(" {root} ").dim()).right_aligned())
        .border_style(Style::new().fg(BORDER));
    if app.filtering() {
        let matches = app.rows.iter().filter(|r| !r.context_only).count();
        block = block.title_bottom(
            Line::from(format!(
                " {matches} match{} ",
                if matches == 1 { "" } else { "es" }
            ))
            .fg(ACCENT),
        );
    }
    if let Some(i) = app.list.selected() {
        block = block.title_bottom(
            Line::from(format!(" {}/{} ", i + 1, app.rows.len()).dim()).right_aligned(),
        );
    }
    app.page = area.height.saturating_sub(2) as usize;

    // Only the root row: say so beneath it.
    let empty = (app.rows.len() <= 1).then(|| -> Vec<Line> {
        if app.filtering() {
            vec!["no matches".dim().into()]
        } else {
            vec![
                "the store is empty".dim().into(),
                "".into(),
                hint(&[("i", "insert"), ("g", "generate"), ("I", "import a file")]),
            ]
        }
    });
    let inner = block.inner(area);

    // A buffer from another context marks nothing in this tree.
    let here = app
        .buffer
        .as_ref()
        .filter(|b| !b.elsewhere(&app.sess.context));
    let cut: Vec<&str> = match here {
        Some(b) if b.cut => b.paths.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let copied: Vec<&str> = match here {
        Some(b) if !b.cut => b.paths.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let renaming = match &app.mode {
        Mode::Rename { path, input } => Some((path.as_str(), input)),
        _ => None,
    };
    let selected = app.list.selected();
    let guides = guides(&app.rows);

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .zip(guides)
        .enumerate()
        .map(|(i, (row, guide))| {
            // The side bar: marked rows get it in the mark color, the
            // highlighted row in the accent (the pill on its name still
            // shows where the cursor is when both apply).
            let bar = if app.marks.contains(&row.path) {
                BAR.fg(MARK)
            } else if selected == Some(i) {
                BAR.fg(ACCENT)
            } else {
                " ".into()
            };
            let flag = if cut.contains(&row.path.as_str()) {
                "✂ ".fg(MARK)
            } else if copied.contains(&row.path.as_str()) {
                "+ ".fg(MARK)
            } else {
                "  ".into()
            };
            let is_file = !row.is_dir && app.sess.files.is_file(&row.path);
            // Every icon is two columns wide; the rename cursor relies on it.
            let icon = match (row.is_dir, app.is_expanded(&row.path)) {
                (true, open) => folder_icon(open).fg(DIR),
                (false, _) if is_file => "▪ ".fg(FILE),
                (false, _) => "• ".dim(),
            };
            let mut spans = vec![bar, flag, guide.dim(), icon];
            match renaming {
                Some((path, input)) if path == row.path => {
                    spans.push(input.text.clone().underlined());
                }
                _ => {
                    let mut name = if row.path.is_empty() {
                        row.name.clone().fg(ACCENT).bold()
                    } else if row.is_dir {
                        format!("{}/", row.name).fg(DIR).bold()
                    } else if is_file {
                        row.name.clone().fg(FILE)
                    } else {
                        row.name.clone().into()
                    };
                    if row.context_only || cut.contains(&row.path.as_str()) {
                        name = name.dim();
                    }
                    if selected == Some(i) {
                        name = name.bold();
                    }
                    spans.push(name);
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(SHADE))
        .scroll_padding(2);
    f.render_stateful_widget(list, area, &mut app.list);
    if let Some(msg) = empty {
        let below = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        let [middle] = Layout::vertical([Constraint::Length(msg.len() as u16)])
            .flex(Flex::Center)
            .areas(below);
        f.render_widget(Paragraph::new(msg).centered(), middle);
    }

    if app.rows.len() > app.page {
        let mut state = ScrollbarState::new(app.rows.len().saturating_sub(app.page))
            .position(app.list.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .symbols(scrollbar::VERTICAL)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::new().dim())
                .thumb_style(Style::new().fg(ACCENT)),
            area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }

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

/// `▰▰▰▱▱`: how much of `total` is left.
fn meter(left: u64, total: u64, cells: usize) -> String {
    let filled = (left * cells as u64)
        .div_ceil(total.max(1))
        .min(cells as u64) as usize;
    "▰".repeat(filled) + &"▱".repeat(cells - filled)
}

fn draw_recent(f: &mut Frame, area: Rect, app: &App) {
    let width = area.width.saturating_sub(2) as usize;
    let clip = app.clip_remaining();
    let mut lines: Vec<Line> = app
        .recent
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let mut spans = vec![format!(" {} ", i + 1).fg(ACCENT).bold()];
            spans.extend(path_spans(path, entry_style(app, path)));
            let mut line = Line::from(spans);
            if let Some((p, secs)) = clip
                && p == path
            {
                // Right-align the clipboard countdown, dropping the meter
                // when space is short.
                let full = format!("{} {secs}s ", meter(secs, app.clip_time.as_secs(), 5));
                let short = format!("{secs}s ");
                let room = width.saturating_sub(line.width() + 2);
                let tail = if full.chars().count() <= room {
                    full
                } else {
                    short
                };
                let gap = width
                    .saturating_sub(line.width() + tail.chars().count())
                    .max(2);
                line.push_span(" ".repeat(gap));
                line.push_span(tail.fg(MARK));
            }
            line
        })
        .collect();
    if lines.is_empty() {
        lines.push(" entries you copy or view land here".dim().into());
        lines.push(Line::from(vec![
            "  ".into(),
            "[1-5]".fg(ACCENT).bold(),
            " use them again".dim(),
        ]));
    }
    f.render_widget(Paragraph::new(lines).block(pane("recent")), area);
}

/// An info-pane row: a dim fixed-width label, then its value.
fn labeled(label: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![format!("{label:<LABEL$}").dim()];
    spans.append(&mut value);
    Line::from(spans)
}

/// The highlighted row's name and a line of detail: the top of the info
/// pane, or the preview pane's title and first line.
fn selection(app: &App) -> (Line<'static>, Line<'static>) {
    let mut lines: Vec<Line> = Vec::new();
    let mut push = |line: Line<'static>| lines.push(line);
    match app.selected() {
        Some(row) if row.is_dir => {
            let root = row.path.is_empty();
            if root {
                push(Line::from(vec![
                    row.name.clone().fg(ACCENT).bold(),
                    format!("  {}", crate::context::tilde(app.sess.store.root())).dim(),
                ]));
            } else {
                let mut name = path_spans(&row.path, Style::new().fg(DIR).bold());
                name.push("/".fg(DIR).bold());
                push(Line::from(name));
            }
            let detail = match app.sess.store.list(&row.path) {
                Ok(children) => {
                    let count = |pred: &dyn Fn(&store::Node) -> bool| {
                        children.iter().filter(|c| pred(c)).count()
                    };
                    let dirs = count(&|c| c.is_dir);
                    let files =
                        count(&|c| !c.is_dir && app.sess.files.is_file(&join(&row.path, &c.name)));
                    let passwords = children.len() - dirs - files;
                    let plural = |n: usize, one: &str, many: &str| {
                        format!("{n} {}", if n == 1 { one } else { many })
                    };
                    let mut parts = vec![
                        if root { "store" } else { "folder" }.to_string(),
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
            push(detail.dim().into());
        }
        Some(row) => {
            let size = app.sess.files.size(&row.path);
            let style = if size.is_some() {
                Style::new().fg(FILE).bold()
            } else {
                Style::new().bold()
            };
            push(Line::from(path_spans(&row.path, style)));
            let mut detail = vec![
                match size {
                    Some(bytes) => format!("file · {}", index::human_size(bytes)),
                    None => "password".to_string(),
                }
                .dim(),
            ];
            if let Some(t) = app.sess.store.modified(&row.path) {
                detail.push(format!(" · modified {}", ago(t)).dim());
            }
            if size.is_some() {
                detail.push("   ".into());
                detail.push("[⏎]".fg(ACCENT).bold());
                detail.push(" export".dim());
            }
            push(Line::from(detail));
        }
        None => {
            push("nothing selected".dim().into());
            push("".into());
        }
    }
    let mut lines = lines.into_iter();
    let name = lines.next().unwrap_or_default();
    (name, lines.next().unwrap_or_default())
}

fn draw_info(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    // Beside the preview, which shows the selection, this pane is left to
    // the session.
    if app.preview.is_none() {
        let (name, detail) = selection(app);
        lines.extend([name, detail]);
    }

    lines.push(labeled(
        "buffer",
        match &app.buffer {
            Some(b) => {
                let names: Vec<&str> = b.paths.iter().map(|p| basename(p)).collect();
                let mut spans = vec![
                    if b.cut { "✂ cut " } else { "+ copied " }.fg(MARK).bold(),
                    names.join(", ").into(),
                ];
                if b.elsewhere(&app.sess.context) {
                    spans.push(" from ".dim());
                    spans.push(b.context.name.clone().fg(ACCENT).bold());
                }
                spans.extend(["   ".into(), "[p]".fg(ACCENT).bold(), " paste".dim()]);
                spans
            }
            None => vec!["empty".dim()],
        },
    ));

    lines.push(labeled(
        "marks",
        if app.marks.is_empty() {
            vec!["none".dim()]
        } else {
            vec![
                format!("{BAR}{} marked", app.marks.len()).fg(MARK).bold(),
                "   ".into(),
                "[x]".fg(ACCENT).bold(),
                " cut ".dim(),
                "[y]".fg(ACCENT).bold(),
                " copy ".dim(),
                "[d]".fg(ACCENT).bold(),
                " delete".dim(),
            ]
        },
    ));

    lines.push(labeled(
        "git",
        match &app.upstream {
            Some(up) if app.ahead > 0 => vec![
                up.clone().into(),
                format!("  ↑{} to push", app.ahead).fg(MARK),
            ],
            Some(up) => vec![up.clone().into(), "  ✓ in sync".fg(ACCENT)],
            None => vec!["no upstream".dim()],
        },
    ));

    if app.preview.is_some() {
        lines.push(labeled(
            "keys",
            if app.sess.keys.is_some() {
                vec!["unlocked".fg(ACCENT)]
            } else {
                vec!["locked".dim()]
            },
        ));
        lines.push(labeled(
            "store",
            vec![
                app.sess.context.name.clone().fg(ACCENT).bold(),
                format!("  {}", crate::context::tilde(app.sess.store.root())).dim(),
            ],
        ));
    }

    f.render_widget(
        Paragraph::new(lines).block(pane("info").padding(Padding::left(1))),
        area,
    );
}

/// The first line of a text entry is the password: masked unless revealed.
/// Later lines (user:, url:, notes) are shown, with `key:` prefixes tinted.
fn secret_lines(text: &str, revealed: bool) -> Vec<Line<'_>> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut out = vec![if revealed {
        first.fg(MARK).bold().into()
    } else {
        MASK.dim().into()
    }];
    if text.lines().nth(1).is_some() {
        out.push("".into());
    }
    out.extend(lines.map(field_line));
    out
}

/// A line after an entry's password, with a `key:` prefix tinted.
fn field_line(line: &str) -> Line<'_> {
    match line.split_once(": ") {
        Some((k, v)) if !k.is_empty() && !k.contains(' ') => {
            Line::from(vec![format!("{k}: ").cyan(), v.into()])
        }
        _ => line.into(),
    }
}

/// The preview pane: the highlighted row's details, who a folder is
/// encrypted to or an entry's lines after its password, and the commits
/// that touched it.
fn draw_preview(f: &mut Frame, area: Rect, app: &App, preview: &Preview) {
    let (name, detail) = selection(app);
    let width = area.width.saturating_sub(4) as usize;
    let mut title = name;
    title.spans.insert(0, " ".into());
    title.push_span(" ");
    let mut lines = vec![detail];
    // The preview loads before each draw, so it is for this row; but say
    // nothing rather than something wrong.
    let current = app.selected().is_some_and(|r| r.path == preview.path());

    match &preview.body {
        _ if !current => {}
        Body::Folder(Ok((from, recipients))) => {
            lines.push("".into());
            let mut label = vec!["encrypted to".dim()];
            if from != preview.path() {
                label.push(format!(" · set in {}", store::display_dir(from)).dim());
            }
            lines.push(Line::from(label));
            for r in recipients {
                let mut spans = vec![
                    "  ".into(),
                    middle_cut(&r.key, width.saturating_sub(6)).into(),
                ];
                if r.you {
                    spans.push(" you".fg(ACCENT));
                }
                lines.push(Line::from(spans));
            }
        }
        Body::Folder(Err(e)) => {
            lines.push("".into());
            lines.push(Line::from(vec![
                "✗ ".fg(DANGER).bold(),
                e.clone().fg(DANGER),
            ]));
        }
        Body::Entry(fields) => {
            lines.push("".into());
            match fields {
                Some(Ok(rest)) => {
                    lines.push(MASK.dim().into());
                    lines.extend(rest.lines().map(field_line));
                }
                Some(Err(e)) => {
                    lines.push(Line::from(vec![
                        "✗ ".fg(DANGER).bold(),
                        e.clone().fg(DANGER),
                    ]));
                }
                None => lines.push(Line::from(vec![
                    MASK.dim(),
                    "   ".into(),
                    "[⏎]".fg(ACCENT).bold(),
                    " unlock".dim(),
                ])),
            }
        }
        Body::File => {}
    }

    if current && !preview.history.is_empty() {
        lines.push("".into());
        lines.push("history".dim().into());
        let room = area.height.saturating_sub(2) as usize;
        for (time, subject) in preview
            .history
            .iter()
            .take(room.saturating_sub(lines.len()))
        {
            let when = format!("{:>3} ", short_ago(commit_time(*time)));
            lines.push(Line::from(vec![
                when.dim(),
                truncate(subject, width.saturating_sub(4)).into(),
            ]));
        }
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title.bold())
        .border_style(Style::new().fg(BORDER))
        .padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// `s` cut to `max` characters by dropping its middle, so both ends of a
/// key stay recognizable.
fn middle_cut(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max || max < 3 {
        return s.to_string();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(len - tail));
    out
}

fn draw_view(f: &mut Frame, app: &App) {
    let Mode::View(view) = &app.mode else {
        return;
    };
    let lines = match &view.secret {
        Secret::Text(text) => secret_lines(text, view.revealed_at.is_some()),
        Secret::File { filename, size } => vec![
            Line::from(vec!["▪ ".fg(FILE), filename.as_str().fg(FILE).bold()]),
            format!("file entry · {}", index::human_size(*size as u64))
                .dim()
                .into(),
        ],
    };
    let mut keys = match &view.secret {
        Secret::Text(_) if view.revealed_at.is_some() => {
            hint_in(&[("space", "hide"), ("c", "copy"), ("esc", "close")], MARK)
        }
        Secret::Text(_) => hint_in(
            &[("space", "reveal"), ("c", "copy"), ("esc", "close")],
            MARK,
        ),
        Secret::File { .. } => hint_in(&[("E", "export"), ("esc", "close")], MARK),
    };
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let mut title = vec![" ".into()];
    title.extend(path_spans(
        &view.path,
        entry_style(app, &view.path).add_modifier(ratatui::style::Modifier::BOLD),
    ));
    title.push(" ".into());
    let mut block = popup(Line::from(title), MARK)
        .title_bottom(keys.right_aligned())
        .padding(Padding::new(2, 2, 1, 1));
    if let Some(t) = view.revealed_at {
        let left = app.clip_time.saturating_sub(t.elapsed()).as_secs() + 1;
        block = block.title_bottom(format!(" hides in {left}s ").fg(MARK));
    } else if let Some((p, secs)) = app.clip_remaining()
        && p == view.path
    {
        block = block.title_bottom(format!(" ⧉ copied · {secs}s ").fg(MARK));
    }
    let width = (f.area().width * 6 / 10).max(44);
    let area = popup_fixed(f.area(), width, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// The footer's height: one row, or up to three for an error that needs
/// them to be read whole.
fn footer_rows(app: &App, width: u16) -> u16 {
    match (&app.mode, &app.status) {
        (Mode::Command(_) | Mode::Filter, _) => 1,
        (_, Some(s)) if s.error => wrapped_rows(&format!("✗ {}", s.text), width).min(3),
        _ => 1,
    }
}

/// How many rows `text` takes word-wrapped to `width` columns.
fn wrapped_rows(text: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let (mut rows, mut col) = (1, 0);
    for word in text.split_whitespace() {
        let w = Span::raw(word).width();
        if col > 0 && col + 1 + w > width {
            rows += 1;
            col = 0;
        }
        col += if col > 0 { 1 } else { 0 } + w;
        // A word wider than the row breaks across rows.
        while col > width {
            rows += 1;
            col -= width;
        }
    }
    rows
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
        Mode::Rename { input, .. } => Some((vec!["rename ".fg(ACCENT).bold()], input)),
        _ => None,
    };
    if let Some((mut spans, input)) = input {
        if let Mode::Rename { .. } = app.mode {
            // The name is edited in the tree; the footer only says how.
            spans.push("⏎".fg(ACCENT).bold());
            spans.push(" save  ".dim());
            spans.push("esc".fg(ACCENT).bold());
            spans.push(" cancel  ".dim());
            spans.push("../ and /abs move it".dim());
            f.render_widget(Paragraph::new(Line::from(spans)), area);
            return;
        }
        let offset: usize = spans.iter().map(|s| s.width()).sum();
        spans.push(input.text.as_str().into());
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        let x = area.x + (offset + input.cursor_col()) as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        return;
    }
    let line = match &app.status {
        Some(s) if s.error => {
            let line = Line::from(vec!["✗ ".fg(DANGER).bold(), s.text.as_str().fg(DANGER)]);
            f.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), area);
            return;
        }
        Some(s) => Line::from(s.text.as_str().fg(ACCENT)),
        None if app.filtering() => {
            let mut spans = vec![
                "/".fg(ACCENT).bold(),
                app.filter
                    .as_ref()
                    .map_or("", |f| f.input.text.as_str())
                    .into(),
                "   ".into(),
            ];
            spans.extend(hint(&[("esc", "clear"), ("/", "edit")]).spans);
            Line::from(spans)
        }
        None => browse_hints(app, area.width as usize),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The footer while browsing: only keys that do something right now, most
/// useful first (`hint_fit` drops from the end), help always last.
fn browse_hints(app: &App, width: usize) -> Line<'static> {
    let mut prefix: Vec<Span> = Vec::new();
    let mut keys: Vec<(&str, &str)> = if !app.marks.is_empty() {
        prefix.push(format!("{} marked  ", app.marks.len()).fg(MARK).bold());
        vec![
            ("x", "cut"),
            ("y", "copy"),
            ("d", "delete"),
            ("␣", "mark more"),
            ("esc", "unmark"),
        ]
    } else if let Some(b) = &app.buffer {
        let n = b.paths.len();
        prefix.push(
            if b.cut {
                format!("✂ {n} cut  ")
            } else {
                format!("+ {n} copied  ")
            }
            .fg(MARK)
            .bold(),
        );
        if b.elsewhere(&app.sess.context) {
            prefix.push(format!("from {}  ", b.context.name).fg(ACCENT).bold());
        }
        vec![
            ("p", "paste here"),
            ("C", "other context"),
            ("esc", "cancel"),
        ]
    } else {
        match app.selected() {
            Some(row) if row.path.is_empty() => vec![
                ("g", "generate"),
                ("i", "insert"),
                ("n", "new folder"),
                ("I", "import file"),
                ("r", "rename context"),
                ("C", "contexts"),
                ("/", "filter"),
                ("u", "history"),
            ],
            Some(row) if row.is_dir => vec![
                (
                    "⏎",
                    if app.is_expanded(&row.path) {
                        "close"
                    } else {
                        "open"
                    },
                ),
                ("g", "generate"),
                ("i", "insert"),
                ("n", "new folder"),
                ("I", "import file"),
                ("␣", "mark"),
                ("r", "rename"),
                ("/", "filter"),
                ("u", "history"),
            ],
            Some(row) if app.sess.files.is_file(&row.path) => vec![
                ("⏎", "export"),
                ("r", "rename"),
                ("d", "delete"),
                ("␣", "mark"),
                ("/", "filter"),
                ("u", "history"),
                (":", "command"),
            ],
            Some(_) => vec![
                ("⏎", "view"),
                ("c", "copy"),
                ("e", "edit"),
                ("r", "rename"),
                ("d", "delete"),
                ("␣", "mark"),
                ("/", "filter"),
                ("u", "history"),
                (":", "command"),
            ],
            None => vec![
                ("i", "insert"),
                ("g", "generate"),
                ("I", "import file"),
                (":", "command"),
            ],
        }
    };
    keys.push(("?", "help"));
    let used: usize = prefix.iter().map(|s| s.width()).sum();
    let mut line = hint_fit(&keys, width.saturating_sub(used));
    prefix.append(&mut line.spans);
    Line::from(prefix)
}

fn hint(keys: &[(&str, &str)]) -> Line<'static> {
    hint_in(keys, ACCENT)
}

/// `[key] what it does` pairs, with the keys in `color`.
fn hint_in(keys: &[(&str, &str)], color: Color) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (k, desc)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push("  ".into());
        }
        spans.push(format!("[{k}]").fg(color).bold());
        spans.push(format!(" {desc}").dim());
    }
    Line::from(spans)
}

/// `hint`, dropping trailing keys that don't fit in `width` columns — but
/// always keeping the last one (help).
fn hint_fit(keys: &[(&str, &str)], width: usize) -> Line<'static> {
    let mut n = keys.len();
    while n > 1 {
        let mut shown: Vec<(&str, &str)> = keys[..n - 1].to_vec();
        shown.push(keys[keys.len() - 1]);
        let line = hint(&shown);
        if line.width() <= width {
            return line;
        }
        n -= 1;
    }
    hint(&keys[keys.len() - 1..])
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A background pull/push: a spinner and git's latest output while it
/// runs; on failure, git's output and the ways out.
fn draw_sync(f: &mut Frame, app: &App, sync: &Sync) {
    let upstream = app.upstream.as_deref().unwrap_or("upstream").to_string();
    let verb = sync.op.verb();
    let shown = if sync.failed { 8 } else { 3 };
    let tail = &sync.output[sync.output.len().saturating_sub(shown)..];

    let mut lines: Vec<Line> = Vec::new();
    let block = if sync.failed {
        lines.push(Line::from(vec![
            "✗ ".fg(DANGER).bold(),
            format!("could not {verb} ").bold(),
            match sync.op {
                Op::Pull => "from ",
                Op::Push => "to ",
            }
            .bold(),
            upstream.fg(DANGER).bold(),
        ]));
        lines.push("".into());
        lines.extend(tail.iter().map(|l| Line::from(l.clone().dim())));
        let mut keys = match sync.op {
            Op::Pull => hint(&[("r", "retry in terminal"), ("esc", "close")]),
            Op::Push => hint(&[
                ("r", "retry in terminal"),
                (
                    "q",
                    if app.switch_to.is_some() {
                        "switch anyway"
                    } else {
                        "quit anyway"
                    },
                ),
                ("esc", "back"),
            ]),
        };
        keys.spans.insert(0, " ".into());
        keys.spans.push(" ".into());
        popup(Line::from(format!(" git {verb} ")).bold(), DANGER).title_bottom(keys.right_aligned())
    } else {
        let elapsed = sync.started.elapsed();
        let frame = (elapsed.as_millis() / 80) as usize % SPINNER.len();
        lines.push(Line::from(vec![
            SPINNER[frame].fg(ACCENT).bold(),
            match sync.op {
                Op::Pull => " pulling from ",
                Op::Push => " pushing to ",
            }
            .into(),
            upstream.bold(),
            format!("  {}s", elapsed.as_secs()).dim(),
        ]));
        lines.push("".into());
        // A steady height while output trickles in.
        for i in 0..shown {
            lines.push(match tail.get(i) {
                Some(l) => Line::from(l.clone().dim()),
                None => "".into(),
            });
        }
        popup(Line::from(format!(" git {verb} ")).bold(), ACCENT)
    };
    let width = (f.area().width * 6 / 10).max(50);
    // Long git errors wrap; size the popup for the wrapped text.
    let text_width = width.saturating_sub(6).max(1) as usize;
    let rows: usize = lines
        .iter()
        .map(|l| l.width().div_ceil(text_width).max(1))
        .sum();
    let area = popup_fixed(f.area(), width, rows as u16 + 4);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block.padding(Padding::new(2, 2, 1, 1))),
        area,
    );
}

fn draw_confirm(f: &mut Frame, app: &App, confirm: &Confirm) {
    let upstream = app.upstream.as_deref();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let (title, question, default_yes) = match confirm {
        Confirm::Delete(targets) => (
            "delete",
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
            "overwrite",
            format!(
                "{conflicts} entr{} already exist{} there — overwrite?",
                if *conflicts == 1 { "y" } else { "ies" },
                if *conflicts == 1 { "s" } else { "" }
            ),
            false,
        ),
        Confirm::Pull => (
            "git pull",
            format!("pull from {}?", upstream.unwrap_or("upstream")),
            true,
        ),
        Confirm::Push { ahead } => (
            "git push",
            format!(
                "push {ahead} commit{} to {}{}?",
                plural(*ahead as usize),
                upstream.unwrap_or("upstream"),
                match &app.switch_to {
                    Some(target) => format!(" before switching to {}", target.name),
                    None => String::new(),
                }
            ),
            true,
        ),
    };
    confirm_box(f, title, &question, default_yes);
}

/// Colors for the graph's lanes, by column, like `git log --graph`.
const LANES: [Color; 6] = [
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Red,
];

/// git's ASCII graph redrawn with box-drawing lines, each lane in its own
/// color, padded to `width` columns.
fn graph_spans(graph: &str, width: usize) -> Vec<Span<'static>> {
    let mut spans: Vec<Span> = graph
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let lane = LANES[(i / 2) % LANES.len()];
            match ch {
                '*' => "●".fg(lane).bold(),
                '|' => "│".fg(lane),
                '/' => "╱".fg(lane),
                '\\' => "╲".fg(lane),
                '_' | '-' => "─".fg(lane),
                '.' => "┄".fg(lane),
                c => Span::raw(c.to_string()),
            }
        })
        .collect();
    let used = graph.chars().count();
    if used < width {
        spans.push(" ".repeat(width - used).into());
    }
    spans
}

/// The history view, laid out like the main one: the commits (with their
/// graph) on top, what the highlighted one changed below.
fn draw_history(f: &mut Frame, app: &mut App) {
    let [top, bottom, footer] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .areas(f.area());
    app.page = top.height.saturating_sub(2) as usize;
    let has_upstream = app.upstream.is_some();
    let files = &app.sess.files;
    let Mode::History(h) = &mut app.mode else {
        return;
    };

    // Commits. Graph, subject and time each get a column, so they line up.
    let width = top.width.saturating_sub(2) as usize;
    let selected = h.list.selected();
    let graph_width = h
        .commits
        .iter()
        .flat_map(|c| std::iter::once(&c.graph).chain(&c.after))
        .map(|g| g.chars().count())
        .max()
        .unwrap_or(0)
        + 1;
    let when_width = h
        .commits
        .iter()
        .map(|c| ago(commit_time(c.time)).chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = h
        .commits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let bar = if selected == Some(i) {
                BAR.fg(ACCENT)
            } else {
                " ".into()
            };
            let flag = if h.unpushed.contains(&c.hash) {
                "↑ ".fg(MARK)
            } else {
                "  ".into()
            };
            let when = format!("{:>when_width$}", ago(commit_time(c.time)));
            let short = format!("{} ", c.short);
            let room = width.saturating_sub(1 + graph_width + 2 + short.len() + when_width + 2);
            let subject = truncate(&c.subject, room);
            let gap = room.saturating_sub(subject.chars().count()) + 2;
            let mut subject = if ["Undo: ", "Redo: ", "Roll back "]
                .iter()
                .any(|p| c.subject.starts_with(p))
            {
                subject.fg(Color::Cyan)
            } else {
                subject.into()
            };
            if selected == Some(i) {
                subject = subject.bold();
            }
            let mut first = vec![bar];
            first.extend(graph_spans(&c.graph, graph_width));
            first.extend([
                flag,
                short.dim(),
                subject,
                " ".repeat(gap).into(),
                when.dim(),
            ]);
            let mut lines = vec![Line::from(first)];
            for g in &c.after {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(graph_spans(g, graph_width));
                lines.push(Line::from(spans));
            }
            ListItem::new(lines)
        })
        .collect();
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(vec![
            " history ".fg(Color::LightMagenta).bg(SHADE).bold(),
            " ".into(),
        ]))
        .border_style(Style::new().fg(BORDER));
    if !h.unpushed.is_empty() {
        block = block.title_top(
            Line::from(format!(" ↑ {} not pushed ", h.unpushed.len()).fg(MARK)).right_aligned(),
        );
    }
    if let Some(i) = selected {
        block = block.title_bottom(
            Line::from(format!(" {}/{} ", i + 1, h.commits.len()).dim()).right_aligned(),
        );
    }
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(SHADE))
            .scroll_padding(2),
        top,
        &mut h.list,
    );

    // What the highlighted commit changed; who and when go on the border.
    let mut block = pane("changes").padding(Padding::horizontal(1));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(c) = h.selected() {
        block = block.title_top(
            Line::from(vec![
                " ".into(),
                c.short.clone().fg(MARK),
                format!(" · {} · {} ", c.author, ago(commit_time(c.time))).dim(),
            ])
            .right_aligned(),
        );
        if has_upstream {
            block = block.title_bottom(
                if h.unpushed.contains(&c.hash) {
                    Line::from(" ↑ not pushed yet ".fg(MARK))
                } else {
                    Line::from(" ✓ pushed ".dim())
                }
                .right_aligned(),
            );
        }
        if c.parents > 1 {
            lines.push("merge — against its first parent:".fg(DIR).into());
        }
        if h.changes.is_empty() {
            lines.push("no file changes".dim().into());
        }
        let rows = bottom.height.saturating_sub(2) as usize;
        let shown = if h.changes.len() + lines.len() > rows {
            rows.saturating_sub(lines.len() + 1)
        } else {
            h.changes.len()
        };
        for change in &h.changes[..shown] {
            let (sign, color) = match change.status {
                'A' | 'C' => ("+ ", ACCENT),
                'D' => ("− ", DANGER),
                'R' => ("→ ", DIR),
                _ => ("~ ", MARK),
            };
            let name = |file: &str| -> Vec<Span<'static>> {
                match history::store_path(file) {
                    Some(p) => {
                        let style = if files.is_file(&p) {
                            Style::new().fg(FILE)
                        } else {
                            Style::new()
                        };
                        path_spans(&p, style)
                    }
                    None => vec![file.to_string().dim()],
                }
            };
            let mut spans = vec![sign.fg(color).bold()];
            spans.extend(name(&change.path));
            if let Some(to) = &change.to {
                spans.push(" → ".fg(DIR));
                spans.extend(name(to));
            }
            lines.push(Line::from(spans));
        }
        if shown < h.changes.len() {
            lines.push(
                format!("… and {} more", h.changes.len() - shown)
                    .dim()
                    .into(),
            );
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), bottom);

    // Footer.
    let line = match &app.status {
        Some(s) if s.error => Line::from(vec!["✗ ".fg(DANGER).bold(), s.text.as_str().fg(DANGER)]),
        Some(s) => Line::from(s.text.as_str().fg(ACCENT)),
        None => hint_fit(
            &[
                ("j k", "move"),
                ("u", "undo this change"),
                ("⏎", "roll back to here"),
                ("esc", "back"),
            ],
            footer.width as usize,
        ),
    };
    f.render_widget(Paragraph::new(line), footer);

    let Mode::History(h) = &app.mode else {
        return;
    };
    if h.confirm
        && let (Some(i), Some(c)) = (h.list.selected(), h.selected())
    {
        let area = f.area();
        f.buffer_mut().set_style(area, Style::new().dim());
        let question = format!(
            "put the store back as it was at \"{}\" ({})? The {i} newer change{} \
             are undone together as one new commit.",
            c.subject,
            ago(commit_time(c.time)),
            if i == 1 { "" } else { "s" },
        );
        confirm_box(f, "roll back", &question, false);
    }
}

fn commit_time(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

/// `s` cut to `max` characters, with an ellipsis if it was longer.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A yes/no question in a popup, colored by its default answer.
fn confirm_box(f: &mut Frame, title: &str, question: &str, default_yes: bool) {
    let color = if default_yes { ACCENT } else { DANGER };
    // The default answer is bracketed in its color; the other is dim.
    let button = |key: &str, label: &str, default: bool| -> Vec<Span<'static>> {
        if default {
            vec![format!("[ {key} {label} ]").fg(color).bold()]
        } else {
            vec![format!("  {key} {label}  ").dim()]
        }
    };
    let mut buttons = button("y", "yes", default_yes);
    buttons.push("   ".into());
    buttons.extend(button("n", "no", !default_yes));

    let width = (question.chars().count() + 8)
        .clamp(36, 72)
        .min(f.area().width as usize) as u16;
    let text_rows = question
        .chars()
        .count()
        .div_ceil(width.saturating_sub(4).max(1) as usize);
    // Word wrapping can take a row more than the plain estimate.
    let text_rows = if text_rows > 1 {
        text_rows + 1
    } else {
        text_rows
    };
    let area = popup_fixed(f.area(), width, text_rows as u16 + 5);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(question.to_string()),
            "".into(),
            Line::from(buttons),
        ])
        .centered()
        .wrap(Wrap { trim: true })
        .block(
            popup(Line::from(format!(" {title} ")).bold(), color).padding(Padding::new(1, 1, 1, 0)),
        ),
        area,
    );
}

fn draw_help(f: &mut Frame) {
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "move",
            &[
                ("j k ↑ ↓", "up / down"),
                ("PgUp PgDn", "a page (Home/End: ends)"),
                ("l →", "expand folder / step into it"),
                ("h ←", "collapse folder / go to parent"),
                ("H ⌫", "collapse the folder you're in"),
                ("-", "collapse everything"),
                ("/", "filter the tree (fuzzy)"),
            ],
        ),
        (
            "use",
            &[
                ("⏎", "view entry (space reveals) · export file"),
                ("c", "copy password"),
                ("1-5", "use a recent entry again"),
                ("e", "edit in $EDITOR"),
                ("E", "export an entry to a file"),
            ],
        ),
        (
            "organize",
            &[
                ("␣ space", "mark / unmark"),
                ("x  y", "cut / copy marked or highlighted"),
                ("p", "paste into highlighted folder"),
                ("C then p", "paste into another context"),
                ("r", "rename in place"),
                ("d", "delete marked or highlighted"),
                ("u", "history: undo or roll back changes"),
                ("esc", "clear filter, marks, then buffer"),
            ],
        ),
        (
            "create",
            &[
                ("g", "generate a password here"),
                ("i", "insert a password here"),
                ("n", "make a folder here"),
                ("I", "import a file here"),
            ],
        ),
        (
            "more",
            &[
                (":", "commands (type to narrow them)"),
                ("C", "switch context (work, personal…)"),
                ("R", "refresh"),
                ("q", "quit (offers to push)"),
            ],
        ),
    ];
    let group_lines = |(name, rows): &(&str, &[(&str, &str)])| -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(name.to_uppercase()).fg(DIR).bold()];
        lines.extend(rows.iter().map(|(k, desc)| {
            Line::from(vec![
                format!("{k:<11}").fg(ACCENT).bold(),
                desc.to_string().into(),
            ])
        }));
        lines.push("".into());
        lines
    };
    let column = |groups: &[(&str, &[(&str, &str)])]| -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = groups.iter().flat_map(group_lines).collect();
        lines.pop();
        lines
    };

    const COL: u16 = 46;
    let screen = f.area();
    let two = screen.width >= 2 * COL + 6;
    let block = popup(Line::from(" keys ").bold(), ACCENT)
        .title_bottom(Line::from(" any key closes ".dim()).right_aligned())
        .padding(Padding::new(2, 2, 1, 1));
    if two {
        let left = column(&groups[..2]);
        let right = column(&groups[2..]);
        let height = left.len().max(right.len()) as u16 + 4;
        let area = popup_fixed(screen, 2 * COL + 6, height);
        f.render_widget(Clear, area);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [l, r] = Layout::horizontal([Constraint::Length(COL), Constraint::Length(COL)])
            .spacing(2)
            .areas(inner);
        f.render_widget(Paragraph::new(left), l);
        f.render_widget(Paragraph::new(right), r);
    } else {
        let lines = column(groups);
        let area = popup_fixed(screen, COL + 6, lines.len() as u16 + 4);
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

/// The command palette, just above the command line: matching commands
/// while the name is typed, then the command's usage and matching paths.
/// At most `ROWS` suggestions, scrolled to keep the selection in view.
fn draw_palette(f: &mut Frame, footer: Rect, palette: &palette::Palette, selected: usize) {
    const ROWS: usize = 8;
    let mut lines: Vec<Line> = Vec::new();
    if let Some(spec) = palette.command {
        lines.push(Line::from(vec![
            format!(":{} ", spec.name).fg(ACCENT).bold(),
            spec.usage.to_string().fg(ACCENT),
            format!("  {}", spec.desc).dim(),
        ]));
    }
    let selected = selected.min(palette.items.len().saturating_sub(1));
    let start = selected.saturating_sub(ROWS - 1);
    let width = footer.width.min(96);
    for (i, s) in palette.items.iter().enumerate().skip(start).take(ROWS) {
        let bar = if i == selected {
            BAR.fg(ACCENT)
        } else {
            " ".into()
        };
        let mut spans = vec![bar];
        match &s.item {
            palette::Item::Command(spec) => {
                spans.push(format!("{:<9}", spec.name).fg(ACCENT).bold());
                let usage = truncate(spec.usage, 34);
                spans.push(format!("{usage:<35}").dim());
                spans.push(spec.desc.into());
            }
            palette::Item::Word { label, note } => {
                spans.push(format!("{label:<9}").fg(ACCENT).bold());
                spans.push(note.clone().dim());
            }
            palette::Item::Path { label, is_dir } => {
                if *is_dir {
                    spans.push(folder_icon(false).fg(DIR));
                    spans.push(label.clone().fg(DIR).bold());
                } else {
                    spans.push("• ".dim());
                    spans.push(label.clone().into());
                }
            }
        }
        let mut line = Line::from(spans);
        if i == selected {
            line = line.bg(SHADE);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return;
    }
    let more = palette.items.len().saturating_sub(start + ROWS);
    let height = (lines.len() as u16 + 2).min(footer.y);
    let area = Rect::new(footer.x, footer.y - height, width, height);
    let mut keys = hint(&[("↑↓", "pick"), ("tab", "complete"), ("⏎", "run")]);
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_bottom(keys.right_aligned());
    if more > 0 {
        block = block.title_bottom(format!(" {more} more ").dim());
    }
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The `g` form.
fn draw_generate(f: &mut Frame, form: &Form) {
    let label = |name: &str, field: Field| -> Vec<Span<'static>> {
        if form.field == field {
            vec![BAR.fg(ACCENT), format!("{name:<8}").fg(ACCENT).bold()]
        } else {
            vec![" ".into(), format!("{name:<8}").dim()]
        }
    };
    // Every option keeps its padding, so nothing shifts as the choice moves.
    let choice = |name: &str, chosen: bool| -> Span<'static> {
        if chosen {
            format!(" {name} ").fg(ACCENT).bg(SHADE).bold()
        } else {
            format!(" {name} ").dim()
        }
    };
    let checkbox = |on: bool| -> Span<'static> {
        if on {
            "[x]".fg(ACCENT).bold()
        } else {
            "[ ]".dim()
        }
    };
    let chosen = Kind::ALL[form.kind];
    let folder = format!("{}/", store::display(&form.folder).trim_end_matches('/'));

    let mut rows: Vec<(Field, Vec<Span<'static>>)> = Vec::new();
    for &field in form.fields() {
        let row = match field {
            Field::Name => {
                let mut row = label("name", field);
                row.push(folder.clone().dim());
                row.push(form.name.text.clone().bold());
                row
            }
            Field::Kind => {
                let mut row = label("type", field);
                for (i, k) in Kind::ALL.iter().enumerate() {
                    row.push(choice(k.name(), i == form.kind));
                }
                row
            }
            Field::Length => {
                let mut row = label("length", field);
                row.push(form.length_input().text.clone().bold());
                row.push(format!("  {}", chosen.unit()).dim());
                if let Some(n) = form.length() {
                    let bits = form.spec().bits(n as usize).floor();
                    // Below 64 bits a password falls to an offline guessing
                    // attack; a PIN is short on purpose, guarded by whatever
                    // asks for it.
                    row.push(if bits < 64.0 && chosen != Kind::Digits {
                        format!(" · {bits} bits, weak").fg(MARK)
                    } else {
                        format!(" · {bits} bits").dim()
                    });
                }
                row
            }
            Field::LookAlikes => {
                let mut row = label("avoid", field);
                let shown: Vec<String> = LOOK_ALIKES.chars().map(String::from).collect();
                row.push(checkbox(form.no_look_alikes));
                row.push(format!(" look-alikes  {}", shown.join(" ")).dim());
                row
            }
            Field::Separator => {
                let mut row = label("between", field);
                for (i, sep) in SEPARATORS.iter().enumerate() {
                    row.push(choice(separator_name(sep), i == form.separator));
                }
                row
            }
            Field::Capitalize => {
                let mut row = label("capitals", field);
                row.push(checkbox(form.capitalize));
                row.push(" Capitalize words, end with a digit".dim());
                row
            }
        };
        rows.push((field, row));
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, (_, row)) in rows.iter().enumerate() {
        if i > 0 {
            lines.push("".into());
        }
        lines.push(Line::from(row.clone()));
    }
    lines.push("".into());
    lines.push(match &form.error {
        Some(e) if form.overwrite => Line::from(e.clone().fg(MARK)),
        Some(e) => Line::from(vec!["✗ ".fg(DANGER).bold(), e.clone().fg(DANGER)]),
        None if form.name.text.trim().is_empty() => {
            "no name: only copied to the clipboard, not saved"
                .dim()
                .into()
        }
        None => "copied to the clipboard, never shown unless you reveal it"
            .dim()
            .into(),
    });

    let mut keys = match form.field {
        Field::Kind | Field::Separator => hint(&[
            ("←→", "choose"),
            ("tab", "next"),
            ("⏎", "generate"),
            ("esc", "cancel"),
        ]),
        Field::Length => hint(&[
            ("←→", "±1"),
            ("tab", "next"),
            ("⏎", "generate"),
            ("esc", "cancel"),
        ]),
        Field::LookAlikes | Field::Capitalize => hint(&[
            ("space", "toggle"),
            ("tab", "next"),
            ("⏎", "generate"),
            ("esc", "cancel"),
        ]),
        Field::Name => hint(&[("tab", "next"), ("⏎", "generate"), ("esc", "cancel")]),
    };
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let block = popup(Line::from(" generate ").bold(), ACCENT)
        .title_bottom(keys.right_aligned())
        .padding(Padding::new(1, 2, 1, 1));
    let width = 82.min(f.area().width);
    // Sized for the type with the most fields, so it doesn't jump as the
    // type changes.
    let most = 5 * 2 + 1;
    let area = popup_fixed(f.area(), width, most + 4);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(Paragraph::new(lines).block(block), area);

    // The cursor sits in whichever text field is focused.
    let Some(row) = rows.iter().position(|(field, _)| *field == form.field) else {
        return;
    };
    let col = match form.field {
        Field::Name => folder.chars().count() + form.name.cursor_col(),
        Field::Length => form.length_input().cursor_col(),
        _ => return,
    };
    let x = inner.x + 1 + 8 + col as u16;
    f.set_cursor_position((
        x.min(inner.right().saturating_sub(1)),
        inner.y + row as u16 * 2,
    ));
}

/// The `C` popup: every context with its store, the current one marked;
/// below, the line for adding one while it is being typed.
fn draw_contexts(f: &mut Frame, app: &App, picker: &Picker) {
    let selected = picker.list.selected();
    let name_width = picker
        .contexts
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);
    let width = 90.min(f.area().width);
    // Bar, mark, name, gaps, "own key", borders and padding.
    let path_width = (width as usize).saturating_sub(name_width + 21);
    let mut lines: Vec<Line> = picker
        .contexts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let here = c.name == app.sess.context.name;
            let highlighted = picker.adding.is_none() && selected == Some(i);
            let mut spans = vec![
                if highlighted {
                    BAR.fg(ACCENT)
                } else {
                    " ".into()
                },
                if here { "● ".fg(ACCENT) } else { "  ".into() },
                {
                    let name = format!("{:<name_width$}", c.name);
                    if highlighted {
                        name.fg(ACCENT).bold()
                    } else {
                        name.bold()
                    }
                },
                format!("  {}", cut_left(crate::context::tilde(&c.path), path_width)).dim(),
            ];
            if c.identity.is_some() {
                spans.push("  own key".fg(DIR));
            }
            Line::from(spans)
        })
        .collect();
    lines.push("".into());
    let mut cursor = None;
    match &picker.adding {
        Some(input) => {
            lines.push(Line::from(vec![
                BAR.fg(ACCENT),
                "new ".fg(ACCENT).bold(),
                input.text.clone().bold(),
            ]));
            cursor = Some((lines.len() - 1, 5 + input.cursor_col()));
            lines.push(
                "a name makes an empty store; name and a path use the store there"
                    .dim()
                    .into(),
            );
        }
        None => lines.push(
            "each context is its own store, synced on its own"
                .dim()
                .into(),
        ),
    }
    if let Some(e) = &picker.error {
        lines.push(Line::from(vec![
            "✗ ".fg(DANGER).bold(),
            e.clone().fg(DANGER),
        ]));
    }
    let mut keys = if picker.adding.is_some() {
        hint(&[("⏎", "add & switch"), ("esc", "cancel")])
    } else {
        hint(&[("⏎", "switch"), ("n", "new"), ("esc", "close")])
    };
    keys.spans.insert(0, " ".into());
    keys.spans.push(" ".into());
    let block = popup(Line::from(" contexts ").bold(), ACCENT)
        .title_bottom(keys.right_aligned())
        .padding(Padding::new(1, 2, 1, 1));
    let area = popup_fixed(f.area(), width, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
    if let Some((row, col)) = cursor {
        let x = inner.x + col as u16;
        f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y + row as u16));
    }
}

/// The store root for the tree's title: `~` for $HOME, and cut from the
/// left to fit `max` columns.
fn short_root(app: &App, max: usize) -> String {
    cut_left(crate::context::tilde(app.sess.store.root()), max)
}

/// `text` cut from the left, behind a `…`, to fit `max` columns.
fn cut_left(text: String, max: usize) -> String {
    let len = text.chars().count();
    if len > max {
        format!(
            "…{}",
            text.chars().skip(len + 1 - max.max(1)).collect::<String>()
        )
    } else {
        text
    }
}

/// A popup's prompt row: `label`, then `len` masking dots shaded across a
/// `popup_width` popup (less its border and padding), then `icon` at the
/// right edge. Dots past the field end in `…`. Also returns the column for
/// a `cursor` that many characters in, kept inside the field.
fn input_line(
    label: &'static str,
    len: usize,
    cursor: usize,
    icon: char,
    popup_width: u16,
) -> (Line<'static>, u16) {
    let w = (popup_width as usize).saturating_sub(2 + 3 + label.len() + 1 + 3);
    let dots = if len > w {
        "•".repeat(w.saturating_sub(1)) + "…"
    } else {
        "•".repeat(len)
    };
    let line = Line::from(vec![
        label.fg(UNLOCK).bold(),
        format!(" {dots:<w$}").bg(FIELD).bold(),
        format!("  {icon}").fg(UNLOCK),
    ]);
    (
        line,
        (label.len() + 1 + cursor.min(w.saturating_sub(1))) as u16,
    )
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

/// "3d"-style age of a timestamp, for narrow columns.
fn short_ago(t: SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let (n, unit) = match secs {
        s if s < 60 => return "now".into(),
        s if s < 3600 => (s / 60, "m"),
        s if s < 86400 => (s / 3600, "h"),
        s if s < 86400 * 60 => (s / 86400, "d"),
        s if s < 86400 * 730 => (s / (86400 * 30), "mo"),
        s => (s / (86400 * 365), "y"),
    };
    format!("{n}{unit}")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(paths: &[(&str, bool)]) -> Vec<Row> {
        paths
            .iter()
            .map(|(p, is_dir)| Row {
                path: p.to_string(),
                name: basename(p).to_string(),
                depth: p.matches('/').count(),
                is_dir: *is_dir,
                context_only: false,
            })
            .collect()
    }

    #[test]
    fn wrapped_rows_match_the_paragraph() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;
        let text = "✗ big.iso is 5242880 bytes (limit 1048576); a password store is a \
                    poor home for large blobs — pass --force to import anyway";
        for width in [20, 37, 60, 80, 119, 200] {
            let area = Rect::new(0, 0, width, 10);
            let mut buf = Buffer::empty(area);
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .render(area, &mut buf);
            let used = (0..10)
                .filter(|&y| (0..width).any(|x| buf[(x, y)].symbol() != " "))
                .count() as u16;
            assert_eq!(wrapped_rows(text, width), used, "width {width}");
        }
    }

    #[test]
    fn tree_guides() {
        let r = rows(&[
            ("a", true),
            ("a/b", true),
            ("a/b/c", false),
            ("a/b/d", false),
            ("a/e", false),
            ("f", false),
        ]);
        assert_eq!(guides(&r), ["", "├─", "│ ├─", "│ └─", "└─", ""]);
    }

    #[test]
    fn countdown_meter() {
        assert_eq!(meter(45, 45, 5), "▰▰▰▰▰");
        assert_eq!(meter(1, 45, 5), "▰▱▱▱▱");
        assert_eq!(meter(23, 45, 5), "▰▰▰▱▱");
    }

    #[test]
    fn hints_drop_from_the_end_but_keep_help() {
        let keys = [("a", "one"), ("b", "two"), ("?", "help")];
        assert_eq!(hint_fit(&keys, 100).width(), hint(&keys).width());
        let narrow = hint_fit(&keys, 20);
        assert_eq!(narrow.width(), hint(&[("a", "one"), ("?", "help")]).width());
    }
}
