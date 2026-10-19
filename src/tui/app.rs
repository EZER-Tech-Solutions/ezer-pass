use super::fuzzy;
use crate::commands::{self, COMMANDS, Session};
use crate::{fileio, index, pass, repl, store};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How many recently used entries the bottom pane keeps (and `1`-`5` copy).
pub const RECENT_MAX: usize = 5;

/// Work the event loop must do with the terminal in hand, because pass may
/// need it (pinentry, $EDITOR, prompts) or print to it.
pub enum Effect {
    None,
    /// Decrypt an entry into the view popup.
    Decrypt(String),
    /// `pass show -c`: copy an entry's first line to the clipboard.
    Copy(String),
    /// Run commands with the terminal handed back to them.
    Run(Batch),
    Pull,
    /// Push, then quit.
    Push,
}

/// Commands to run one after another, stopping at the first failure.
pub struct Batch {
    pub jobs: Vec<Job>,
    pub pause: Pause,
    /// Row to highlight afterwards (a rename's or paste's destination).
    select: Option<String>,
    /// Empty the cut buffer once every job succeeded.
    clear_buffer: bool,
}

impl Batch {
    fn new(jobs: Vec<Job>, pause: Pause) -> Self {
        Batch {
            jobs,
            pause,
            select: None,
            clear_buffer: false,
        }
    }
}

pub struct Job {
    pub argv: Vec<String>,
    /// A store path this job renames, so marks, recents and expanded
    /// folders can follow it.
    moved: Option<(String, String)>,
}

impl Job {
    fn new(argv: Vec<String>) -> Self {
        Job { argv, moved: None }
    }
}

/// Whether to hold the screen after a `Run` so its output can be read.
pub enum Pause {
    Always,
    OnError,
}

/// One visible line of the tree.
pub struct Row {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    /// Shown only as the ancestor of a filter match.
    pub context_only: bool,
}

/// Entries cut (`x`) or copied (`y`), waiting for `p`.
pub struct Buffer {
    pub cut: bool,
    pub paths: Vec<String>,
}

/// The `/` filter. `index` is every store path, walked when the filter
/// opened, so typing doesn't hit the filesystem.
pub struct Filter {
    pub input: Input,
    index: Vec<(String, bool)>,
}

pub enum Secret {
    Text(String),
    File { filename: String, size: usize },
}

/// The decrypted entry in the `v` popup. Dropped when the popup closes, so
/// plaintext is held only while it is on screen.
pub struct View {
    pub path: String,
    pub secret: Secret,
    /// When the password was unmasked; it re-masks after `clip_time`.
    pub revealed_at: Option<Instant>,
}

pub enum Confirm {
    Delete(Vec<(String, bool)>),
    Overwrite { batch: Batch, conflicts: usize },
    Pull,
    Push { ahead: u64 },
}

pub enum Mode {
    Browse,
    /// Typing into the `/` filter.
    Filter,
    Command(Input),
    Rename {
        path: String,
        input: Input,
    },
    Confirm(Confirm),
    View(View),
    Help,
}

pub struct Status {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub sess: Session,
    pub rows: Vec<Row>,
    pub list: ListState,
    expanded: HashSet<String>,
    pub marks: HashSet<String>,
    pub buffer: Option<Buffer>,
    pub filter: Option<Filter>,
    pub mode: Mode,
    pub status: Option<Status>,
    /// Recently copied or viewed entries, newest first.
    pub recent: Vec<String>,
    /// The entry last copied to the clipboard, and when.
    pub last_copy: Option<(String, Instant)>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub quit: bool,
    pub clip_time: Duration,
    /// Rows the tree shows at once, set by each draw; PgUp/PgDn move by it.
    pub page: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl App {
    pub fn new(sess: Session) -> Self {
        let clip_time = std::env::var("PASSWORD_STORE_CLIP_TIME")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(45);
        let recent = repl::state_file("recent")
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.lines()
                    .filter(|p| sess.store.is_entry(p))
                    .take(RECENT_MAX)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut app = App {
            sess,
            rows: Vec::new(),
            list: ListState::default(),
            expanded: HashSet::new(),
            marks: HashSet::new(),
            buffer: None,
            filter: None,
            mode: Mode::Browse,
            status: None,
            recent,
            last_copy: None,
            upstream: None,
            ahead: 0,
            quit: false,
            clip_time: Duration::from_secs(clip_time),
            page: 10,
            history: Vec::new(),
            history_pos: None,
        };
        app.refresh_git();
        if app.upstream.is_some() {
            app.mode = Mode::Confirm(Confirm::Pull);
        }
        app.rebuild(None);
        app
    }

    pub fn selected(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.rows.get(i))
    }

    fn selected_path(&self) -> Option<String> {
        self.selected().map(|r| r.path.clone())
    }

    fn selected_entry(&self) -> Option<String> {
        self.selected()
            .filter(|r| !r.is_dir)
            .map(|r| r.path.clone())
    }

    pub fn is_expanded(&self, path: &str) -> bool {
        self.filtering() || self.expanded.contains(path)
    }

    /// Whether a non-empty filter is narrowing the tree.
    pub fn filtering(&self) -> bool {
        self.filter
            .as_ref()
            .is_some_and(|f| !f.input.text.trim().is_empty())
    }

    /// The directory new things go into: the highlighted folder, or the
    /// folder holding the highlighted entry.
    fn context_dir(&self) -> String {
        match self.selected() {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => parent(&r.path).to_string(),
            None => String::new(),
        }
    }

    /// Recompute the visible rows, highlighting `select` if given, else
    /// keeping the current row (or its index if it disappeared).
    fn rebuild(&mut self, select: Option<String>) {
        let keep = select.or_else(|| self.selected_path());
        let old_index = self.list.selected().unwrap_or(0);
        let mut rows = Vec::new();
        match &self.filter {
            Some(f) if self.filtering() => {
                let mut shown = HashSet::new();
                let mut matched = HashSet::new();
                for (path, _) in &f.index {
                    if fuzzy::score(&f.input.text, path).is_some() {
                        matched.insert(path.as_str());
                        let mut p = path.as_str();
                        while !p.is_empty() && shown.insert(p) {
                            p = parent(p);
                        }
                    }
                }
                for (path, is_dir) in &f.index {
                    if shown.contains(path.as_str()) {
                        rows.push(Row {
                            name: basename(path).to_string(),
                            depth: path.matches('/').count(),
                            is_dir: *is_dir,
                            context_only: !matched.contains(path.as_str()),
                            path: path.clone(),
                        });
                    }
                }
            }
            _ => tree_rows(&self.sess.store, &self.expanded, "", 0, &mut rows),
        }
        self.rows = rows;
        let index = keep
            .and_then(|p| self.rows.iter().position(|r| r.path == p))
            .unwrap_or(old_index);
        self.select(index);
    }

    fn select(&mut self, index: usize) {
        if self.rows.is_empty() {
            self.list.select(None);
        } else {
            self.list.select(Some(index.min(self.rows.len() - 1)));
        }
    }

    fn move_by(&mut self, delta: isize) {
        let index = self.list.selected().unwrap_or(0);
        self.select(index.saturating_add_signed(delta));
    }

    /// Expand every folder above `path` so its row is visible.
    fn reveal_path(&mut self, path: &str) {
        let mut p = parent(path);
        while !p.is_empty() {
            self.expanded.insert(p.to_string());
            p = parent(p);
        }
    }

    /// Highlight a store path, expanding folders (and dropping a filter
    /// that hides it) as needed.
    fn go_to(&mut self, path: &str) {
        if self.filtering() && !self.rows.iter().any(|r| r.path == path) {
            self.filter = None;
        }
        self.reveal_path(path);
        self.rebuild(Some(path.to_string()));
    }

    pub fn refresh_git(&mut self) {
        self.upstream = pass::git_upstream(self.sess.store.root());
        self.ahead = if self.upstream.is_some() {
            pass::git_ahead_count(self.sess.store.root())
        } else {
            0
        };
    }

    /// Forget paths that no longer exist (deleted, or moved outside the TUI).
    fn prune(&mut self) {
        let store = &self.sess.store;
        let exists = |p: &String| store.is_entry(p) || store.is_dir(p);
        self.marks.retain(exists);
        self.expanded.retain(|p| store.is_dir(p));
        let before = self.recent.len();
        self.recent.retain(|p| store.is_entry(p));
        if self.recent.len() != before {
            self.save_recent();
        }
        if let Some(buf) = &mut self.buffer {
            buf.paths.retain(exists);
            if buf.paths.is_empty() {
                self.buffer = None;
            }
        }
    }

    /// Rewrite every remembered path under `from` to live under `to`.
    fn moved(&mut self, from: &str, to: &str) {
        let rewrite = |p: &String| -> String {
            if p == from {
                to.to_string()
            } else if let Some(rest) = p.strip_prefix(&format!("{from}/")) {
                format!("{to}/{rest}")
            } else {
                p.clone()
            }
        };
        self.expanded = self.expanded.iter().map(rewrite).collect();
        self.marks = self.marks.iter().map(rewrite).collect();
        self.recent = self.recent.iter().map(rewrite).collect();
        if let Some(buf) = &mut self.buffer {
            buf.paths = buf.paths.iter().map(rewrite).collect();
        }
        if let Some((path, _)) = &mut self.last_copy {
            *path = rewrite(path);
        }
        self.save_recent();
    }

    fn push_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(RECENT_MAX);
        self.save_recent();
    }

    /// Persist recents so `1`-`5` work across sessions. Store paths only;
    /// pass doesn't encrypt names either.
    fn save_recent(&self) {
        if let Some(path) = repl::state_file("recent") {
            let _ = std::fs::write(path, self.recent.join("\n") + "\n");
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// Re-mask a revealed password once the clipboard timeout has passed.
    pub fn tick(&mut self) {
        if let Mode::View(view) = &mut self.mode
            && view
                .revealed_at
                .is_some_and(|t| t.elapsed() >= self.clip_time)
        {
            view.revealed_at = None;
        }
    }

    /// Seconds until the clipboard clears, if the last copy is still live.
    pub fn clip_remaining(&self) -> Option<(&str, u64)> {
        let (path, at) = self.last_copy.as_ref()?;
        let left = self.clip_time.checked_sub(at.elapsed())?;
        Some((path, left.as_secs() + 1))
    }

    fn request_quit(&mut self) {
        if self.upstream.is_some() {
            self.refresh_git();
        }
        if self.upstream.is_some() && self.ahead > 0 {
            self.mode = Mode::Confirm(Confirm::Push { ahead: self.ahead });
        } else {
            self.quit = true;
        }
    }

    pub fn finish_decrypt(&mut self, path: String, result: Result<String>) {
        let content = match result {
            Ok(content) => content,
            Err(e) => return self.error(format!("{e:#}")),
        };
        let secret = match fileio::decode_payload(&content) {
            Ok(file) => Secret::File {
                filename: file.filename,
                size: file.data.len(),
            },
            Err(_) if fileio::marker_filename(&content).is_some() => {
                return self.error("file entry is corrupt");
            }
            Err(_) => Secret::Text(content),
        };
        // Decrypting is the one moment the truth is known; fix the index
        // if it was wrong (e.g. after a plain `pass mv`).
        let size = match &secret {
            Secret::File { size, .. } => Some(*size as u64),
            Secret::Text(_) => None,
        };
        self.sess.files.learn(&path, size);
        self.sess.files.prune(|p| self.sess.store.is_entry(p));
        self.sess.files.save(self.sess.store.root());
        self.push_recent(&path);
        self.mode = Mode::View(View {
            path,
            secret,
            revealed_at: None,
        });
    }

    pub fn finish_copy(&mut self, path: &str, result: Result<()>) {
        match result {
            Ok(()) => {
                self.push_recent(path);
                self.last_copy = Some((path.to_string(), Instant::now()));
                self.info(format!("copied {} to clipboard", store::display(path)));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// After a batch ran with the terminal: apply the renames that went
    /// through and re-read everything the commands may have changed.
    /// Re-read the file index; a pull may have brought a new one.
    fn reload_files(&mut self) {
        self.sess.files = index::FileIndex::load(self.sess.store.root());
    }

    pub fn finish_run(&mut self, batch: &Batch, done: usize, result: Result<()>) {
        self.reload_files();
        for job in &batch.jobs[..done] {
            if let Some((from, to)) = &job.moved {
                self.moved(from, to);
            }
        }
        if result.is_ok() && batch.clear_buffer {
            self.buffer = None;
        }
        self.prune();
        if let Some(f) = &mut self.filter {
            f.index = self.sess.store.walk();
        }
        match &batch.select {
            Some(path) if result.is_ok() => self.go_to(path),
            _ => self.rebuild(None),
        }
        self.refresh_git();
        if let Err(e) = result {
            self.error(format!("{e:#}"));
        }
    }

    pub fn finish_sync(&mut self, what: &str, result: Result<()>) {
        self.reload_files();
        self.prune();
        self.rebuild(None);
        self.refresh_git();
        match result {
            Ok(()) => self.info(what),
            Err(e) => self.error(format!("git {e:#}")),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.mode {
            Mode::Browse if ctrl && key.code == KeyCode::Char('c') => {
                self.request_quit();
                Effect::None
            }
            Mode::Browse => self.browse_key(key),
            Mode::Filter => self.filter_key(key),
            Mode::Command(_) => self.command_key(key),
            Mode::Rename { .. } => self.rename_key(key),
            Mode::Confirm(_) => self.confirm_key(key),
            Mode::View(_) => self.view_key(key),
            Mode::Help => {
                self.mode = Mode::Browse;
                Effect::None
            }
        }
    }

    fn browse_key(&mut self, key: KeyEvent) -> Effect {
        self.status = None;
        let row = self.selected().map(|r| (r.path.clone(), r.is_dir, r.depth));
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(self.page.max(1) as isize),
            KeyCode::PageUp => self.move_by(-(self.page.max(1) as isize)),
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some((path, true, depth)) = row {
                    if !self.is_expanded(&path) {
                        self.expanded.insert(path);
                        self.rebuild(None);
                    } else if let Some(i) = self.list.selected()
                        && self.rows.get(i + 1).is_some_and(|r| r.depth > depth)
                    {
                        self.select(i + 1);
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some((path, is_dir, _)) = row {
                    if is_dir && !self.filtering() && self.expanded.remove(&path) {
                        self.rebuild(None);
                    } else if let Some(i) = self.rows.iter().position(|r| r.path == parent(&path)) {
                        self.select(i);
                    }
                }
            }
            // Fold up the folder the cursor is inside, wherever in it the
            // cursor sits, and land on that folder.
            KeyCode::Char('H') | KeyCode::Backspace => {
                if let Some((path, ..)) = row {
                    let section = parent(&path).to_string();
                    if !section.is_empty() {
                        self.expanded.remove(&section);
                        self.rebuild(Some(section));
                    }
                }
            }
            KeyCode::Enter => match row {
                Some((path, true, _)) if self.filtering() => self.go_to_dir(&path),
                Some((path, true, _)) => {
                    if !self.expanded.remove(&path) {
                        self.expanded.insert(path);
                    }
                    self.rebuild(None);
                }
                Some((path, false, _)) => return self.use_entry(path),
                None => {}
            },
            KeyCode::Char('c') => {
                if let Some(path) = self.selected_entry() {
                    if self.sess.files.is_file(&path) {
                        self.error(format!(
                            "{} is a file — ⏎ or E exports it",
                            store::display(&path)
                        ));
                    } else {
                        return Effect::Copy(path);
                    }
                }
            }
            KeyCode::Char('v') => {
                if let Some(path) = self.selected_entry() {
                    return Effect::Decrypt(path);
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let n = c as usize - '1' as usize;
                if let Some(path) = self.recent.get(n) {
                    return self.use_entry(path.clone());
                }
            }
            KeyCode::Char(' ') => {
                if let Some((path, _, _)) = row {
                    if !self.marks.remove(&path) {
                        self.marks.insert(path);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('x') => self.fill_buffer(true),
            KeyCode::Char('y') => self.fill_buffer(false),
            KeyCode::Char('p') => return self.paste(),
            KeyCode::Char('r') => {
                if let Some((path, _, _)) = row {
                    let input = Input::new(basename(&path).to_string());
                    self.mode = Mode::Rename { path, input };
                }
            }
            KeyCode::Char('d') => {
                let targets = self.targets();
                if !targets.is_empty() {
                    let targets = targets
                        .into_iter()
                        .map(|p| {
                            let is_dir = self.sess.store.is_dir(&p);
                            (p, is_dir)
                        })
                        .collect();
                    self.mode = Mode::Confirm(Confirm::Delete(targets));
                }
            }
            KeyCode::Char('e') => {
                if let Some(path) = self.selected_entry() {
                    let job = Job::new(vec!["edit".into(), store::display(&path)]);
                    return Effect::Run(Batch::new(vec![job], Pause::OnError));
                }
            }
            KeyCode::Char('E') => {
                if let Some(path) = self.selected_entry() {
                    self.export_prompt(&path);
                }
            }
            KeyCode::Char('g') => self.prefill("generate ".into()),
            KeyCode::Char('i') => self.prefill("insert ".into()),
            KeyCode::Char('m') => self.prefill("mkdir ".into()),
            KeyCode::Char('I') => self.prefill("import ".into()),
            KeyCode::Char(':') => self.prefill(String::new()),
            KeyCode::Char('-') => {
                self.expanded.clear();
                let top = row.map(|(p, ..)| p.split('/').next().unwrap_or("").to_string());
                self.rebuild(top);
            }
            KeyCode::Char('/') => {
                if self.filter.is_none() {
                    self.filter = Some(Filter {
                        input: Input::default(),
                        index: self.sess.store.walk(),
                    });
                }
                self.mode = Mode::Filter;
            }
            // Esc peels back one layer of state at a time.
            KeyCode::Esc => {
                if self.filter.is_some() {
                    self.filter = None;
                    if let Some(path) = self.selected_path() {
                        self.go_to(&path);
                    } else {
                        self.rebuild(None);
                    }
                } else if !self.marks.is_empty() {
                    self.marks.clear();
                } else if self.buffer.take().is_some() {
                    self.info("buffer cleared");
                }
            }
            KeyCode::Char('R') => {
                self.reload_files();
                self.prune();
                if let Some(f) = &mut self.filter {
                    f.index = self.sess.store.walk();
                }
                self.rebuild(None);
                self.refresh_git();
                self.info("refreshed");
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Effect::None
    }

    /// Enter on a folder while filtering: drop the filter and open it.
    fn go_to_dir(&mut self, path: &str) {
        self.filter = None;
        self.expanded.insert(path.to_string());
        self.go_to(path);
    }

    /// What x/y/d act on: the marked rows, else the highlighted one. A
    /// marked folder covers anything marked inside it.
    fn targets(&self) -> Vec<String> {
        if self.marks.is_empty() {
            return self.selected_path().into_iter().collect();
        }
        let mut paths: Vec<String> = self.marks.iter().cloned().collect();
        paths.sort();
        let mut out: Vec<String> = Vec::new();
        for p in paths {
            if !out.iter().any(|o| p.starts_with(&format!("{o}/"))) {
                out.push(p);
            }
        }
        out
    }

    fn fill_buffer(&mut self, cut: bool) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.marks.clear();
        self.buffer = Some(Buffer { cut, paths });
        self.info(format!(
            "{} {n} item{} — p to paste into the highlighted folder",
            if cut { "cut" } else { "copied" },
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Plan moving (cut) or copying the buffer into the context folder.
    /// Refuses the whole paste on any hard conflict; overwriting existing
    /// entries asks first.
    fn paste(&mut self) -> Effect {
        let Some(buf) = &self.buffer else {
            self.error("nothing to paste (x cuts, y copies)");
            return Effect::None;
        };
        let store = &self.sess.store;
        let target = self.context_dir();
        let cut = buf.cut;
        let op = if cut { "mv" } else { "cp" };
        let mut jobs = Vec::new();
        let mut dests = HashSet::new();
        let mut conflicts = 0;
        let mut select = None;
        for src in &buf.paths {
            let is_dir = store.is_dir(src);
            let mut fail = |msg: String| -> Effect {
                self.status = Some(Status {
                    text: msg,
                    error: true,
                });
                Effect::None
            };
            if !is_dir && !store.is_entry(src) {
                return fail(format!("{} no longer exists", store::display(src)));
            }
            if is_dir && (target == *src || target.starts_with(&format!("{src}/"))) {
                return fail(format!("cannot paste {} into itself", store::display(src)));
            }
            let mut dest = join(&target, basename(src));
            if dest == *src {
                if cut {
                    continue;
                }
                dest = free_copy_name(store, &dest);
            } else if store.is_dir(&dest) || (is_dir && store.is_entry(&dest)) {
                return fail(format!("{} already exists", store::display(&dest)));
            } else if store.is_entry(&dest) {
                conflicts += 1;
            }
            if !dests.insert(dest.clone()) {
                return fail(format!("two items would land at {}", store::display(&dest)));
            }
            select.get_or_insert_with(|| dest.clone());
            jobs.push(Job {
                argv: vec![
                    op.into(),
                    "-f".into(),
                    store::display(src),
                    store::display(&dest),
                ],
                moved: cut.then(|| (src.clone(), dest.clone())),
            });
        }
        if jobs.is_empty() {
            self.info("already here");
            return Effect::None;
        }
        let batch = Batch {
            jobs,
            pause: Pause::OnError,
            select,
            clear_buffer: cut,
        };
        if conflicts > 0 {
            self.mode = Mode::Confirm(Confirm::Overwrite { batch, conflicts });
            return Effect::None;
        }
        Effect::Run(batch)
    }

    /// The default action on an entry: copy a password, or start exporting
    /// a file (copying would only grab its marker line).
    fn use_entry(&mut self, path: String) -> Effect {
        if self.sess.files.is_file(&path) {
            self.export_prompt(&path);
            Effect::None
        } else {
            Effect::Copy(path)
        }
    }

    /// `:export <entry> ` with the cursor ready for a destination; Enter
    /// right away writes the original filename to the current directory.
    fn export_prompt(&mut self, path: &str) {
        self.sess.cwd = parent(path).to_string();
        self.history_pos = None;
        self.mode = Mode::Command(Input::new(format!("export {} ", quote(basename(path)))));
    }

    /// Open the command line, run relative to the context folder.
    fn prefill(&mut self, text: String) {
        self.sess.cwd = self.context_dir();
        self.history_pos = None;
        self.mode = Mode::Command(Input::new(text));
    }

    fn filter_key(&mut self, key: KeyEvent) -> Effect {
        let Some(filter) = &mut self.filter else {
            self.mode = Mode::Browse;
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                return self.browse_key(key);
            }
            KeyCode::Char('c') if ctrl => {
                self.mode = Mode::Browse;
                return self.browse_key(KeyEvent::from(KeyCode::Esc));
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.mode = Mode::Browse;
                if !self.filtering() {
                    self.filter = None;
                }
            }
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            _ => {
                if filter.input.handle(key) {
                    let best = filter
                        .index
                        .iter()
                        .filter_map(|(p, _)| fuzzy::score(&filter.input.text, p).map(|s| (s, p)))
                        .max_by(|(sa, a), (sb, b)| sa.cmp(sb).then(b.len().cmp(&a.len())))
                        .map(|(_, p)| p.clone());
                    self.rebuild(best);
                }
            }
        }
        Effect::None
    }

    fn rename_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Rename { path, input } = &mut self.mode else {
            return Effect::None;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = Mode::Browse
            }
            KeyCode::Enter => {
                let src = path.clone();
                let name = input.text.trim().to_string();
                self.mode = Mode::Browse;
                if name.is_empty() || name == basename(&src) {
                    return Effect::None;
                }
                // Relative to the entry's folder, so `../x` and `/a/b` work too.
                let dest = store::resolve(parent(&src), &name);
                let store = &self.sess.store;
                if dest.is_empty() || store.is_entry(&dest) || store.is_dir(&dest) {
                    self.error(format!("{} already exists", store::display(&dest)));
                    return Effect::None;
                }
                if dest.starts_with(&format!("{src}/")) {
                    self.error("cannot move a folder into itself");
                    return Effect::None;
                }
                let job = Job {
                    argv: vec!["mv".into(), store::display(&src), store::display(&dest)],
                    moved: Some((src, dest.clone())),
                };
                let mut batch = Batch::new(vec![job], Pause::OnError);
                batch.select = Some(dest);
                return Effect::Run(batch);
            }
            _ => {
                input.handle(key);
            }
        }
        Effect::None
    }

    fn view_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::View(view) = &mut self.mode else {
            return Effect::None;
        };
        match key.code {
            KeyCode::Char(' ') => {
                view.revealed_at = match view.revealed_at {
                    Some(_) => None,
                    None => Some(Instant::now()),
                };
            }
            KeyCode::Char('c') if matches!(view.secret, Secret::Text(_)) => {
                let path = view.path.clone();
                self.mode = Mode::Browse;
                return Effect::Copy(path);
            }
            KeyCode::Char('E' | 'x') if matches!(view.secret, Secret::File { .. }) => {
                let path = view.path.clone();
                self.export_prompt(&path);
            }
            _ => self.mode = Mode::Browse,
        }
        Effect::None
    }

    fn command_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Command(input) = &mut self.mode else {
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Char('c') if ctrl => self.mode = Mode::Browse,
            KeyCode::Backspace if input.text.is_empty() => self.mode = Mode::Browse,
            KeyCode::Enter => {
                let line = input.text.trim().to_string();
                self.mode = Mode::Browse;
                self.status = None;
                if line.is_empty() {
                    return Effect::None;
                }
                if self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                return self.run_line(&line);
            }
            KeyCode::Up if !self.history.is_empty() => {
                let pos = self
                    .history_pos
                    .map_or(self.history.len() - 1, |p| p.saturating_sub(1));
                self.history_pos = Some(pos);
                *input = Input::new(self.history[pos].clone());
            }
            KeyCode::Down if self.history_pos.is_some() => {
                let pos = self.history_pos.unwrap() + 1;
                if pos < self.history.len() {
                    self.history_pos = Some(pos);
                    *input = Input::new(self.history[pos].clone());
                } else {
                    self.history_pos = None;
                    *input = Input::default();
                }
            }
            _ => {
                input.handle(key);
            }
        }
        Effect::None
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Confirm(confirm) = &self.mode else {
            return Effect::None;
        };
        let default_yes = matches!(confirm, Confirm::Pull | Confirm::Push { .. });
        let yes = match key.code {
            KeyCode::Char('y' | 'Y') => true,
            KeyCode::Enter => default_yes,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => false,
            _ => return Effect::None,
        };
        let Mode::Confirm(confirm) = std::mem::replace(&mut self.mode, Mode::Browse) else {
            unreachable!()
        };
        match (confirm, yes) {
            (Confirm::Delete(targets), true) => {
                let jobs = targets
                    .into_iter()
                    .map(|(path, is_dir)| {
                        let mut argv = vec!["rm".to_string(), "-f".to_string()];
                        if is_dir {
                            argv.push("-r".into());
                        }
                        argv.push(store::display(&path));
                        Job::new(argv)
                    })
                    .collect();
                self.marks.clear();
                Effect::Run(Batch::new(jobs, Pause::OnError))
            }
            (Confirm::Overwrite { batch, .. }, true) => Effect::Run(batch),
            (Confirm::Pull, true) => Effect::Pull,
            (Confirm::Pull, false) => {
                self.info("not pulled (pull later with `:git pull`)");
                Effect::None
            }
            (Confirm::Push { .. }, true) => Effect::Push,
            (Confirm::Push { .. }, false) => {
                self.quit = true;
                Effect::None
            }
            (Confirm::Delete(_) | Confirm::Overwrite { .. }, false) => Effect::None,
        }
    }

    /// Run a `:` command line in the context folder. Navigation and
    /// anything that would print a secret are handled in the TUI; the rest
    /// go to `commands::run` with the terminal handed over.
    fn run_line(&mut self, line: &str) -> Effect {
        let argv = match commands::tokenize(line) {
            Ok(argv) if !argv.is_empty() => argv,
            Ok(_) => return Effect::None,
            Err(e) => {
                self.error(format!("{e:#}"));
                return Effect::None;
            }
        };
        let cmd = argv[0].as_str();
        let (flags, positional): (Vec<&str>, Vec<&str>) = argv[1..]
            .iter()
            .map(String::as_str)
            .partition(|a| a.starts_with('-') && a.len() > 1);
        let store = &self.sess.store;
        match cmd {
            "exit" | "quit" => self.request_quit(),
            "help" | "?" => self.mode = Mode::Help,
            "pwd" => self.info(store::display(&self.sess.cwd)),
            "cd" | "ls" | "list" if positional.len() <= 1 => {
                let path = self
                    .sess
                    .resolve(positional.first().copied().unwrap_or("."));
                if path.is_empty() {
                    self.go_to(
                        self.rows
                            .first()
                            .map(|r| r.path.clone())
                            .as_deref()
                            .unwrap_or(""),
                    );
                } else if store.is_dir(&path) {
                    self.go_to_dir(&path);
                } else if store.is_entry(&path) {
                    self.go_to(&path);
                } else {
                    self.error(format!(
                        "{} is not in the password store",
                        store::display(&path)
                    ));
                }
            }
            "mkdir" if positional.len() == 1 => {
                let path = self.sess.resolve(positional[0]);
                match store.mkdir(&path) {
                    Ok(()) => {
                        if let Some(f) = &mut self.filter {
                            f.index = self.sess.store.walk();
                        }
                        self.go_to(&path);
                        self.info(format!("created {}/", store::display(&path)));
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            // Show in the popup rather than printing plaintext.
            "show" if positional.len() == 1 => {
                let path = self.sess.resolve(positional[0]);
                if !store.is_entry(&path) {
                    self.error(format!(
                        "{} is not in the password store",
                        store::display(&path)
                    ));
                    return Effect::None;
                }
                self.go_to(&path);
                return if flags.iter().any(|f| *f == "-c" || *f == "--clip") {
                    Effect::Copy(path)
                } else {
                    Effect::Decrypt(path)
                };
            }
            _ if COMMANDS.contains(&cmd) => {
                // Quiet commands return straight to the TUI; anything that
                // prints (generate, find, git log, ...) holds for a keypress.
                let pause = match cmd {
                    "edit" | "insert" | "mv" | "cp" | "rm" => Pause::OnError,
                    _ => Pause::Always,
                };
                return Effect::Run(Batch::new(vec![Job::new(argv)], pause));
            }
            _ => self.error(format!("unknown command {cmd:?} (`?` for help)")),
        }
        Effect::None
    }
}

/// Append the rows for `dir`'s contents, recursing into expanded folders.
fn tree_rows(
    store: &store::Store,
    expanded: &HashSet<String>,
    dir: &str,
    depth: usize,
    out: &mut Vec<Row>,
) {
    let Ok(nodes) = store.list(dir) else { return };
    for node in nodes {
        let path = join(dir, &node.name);
        let open = node.is_dir && expanded.contains(&path);
        out.push(Row {
            path: path.clone(),
            name: node.name,
            depth,
            is_dir: node.is_dir,
            context_only: false,
        });
        if open {
            tree_rows(store, expanded, &path, depth + 1, out);
        }
    }
}

/// The folder holding a store path ("" for top-level items).
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

pub fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `name-copy`, `name-copy2`, ... — the first that doesn't exist yet.
fn free_copy_name(store: &store::Store, path: &str) -> String {
    (1..)
        .map(|n| match n {
            1 => format!("{path}-copy"),
            n => format!("{path}-copy{n}"),
        })
        .find(|p| !store.is_entry(p) && !store.is_dir(p))
        .expect("unbounded")
}

/// Quote a name for the command line if `commands::tokenize` would split
/// or unescape it.
fn quote(name: &str) -> String {
    if !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
    {
        return name.to_string();
    }
    format!("'{}'", name.replace('\'', "'\\''"))
}

/// A single-line text input with a cursor.
#[derive(Default)]
pub struct Input {
    pub text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
}

impl Input {
    pub fn new(text: String) -> Self {
        let cursor = text.len();
        Input { text, cursor }
    }

    /// Cursor position in characters, for placing the terminal cursor.
    pub fn cursor_col(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .chars()
            .next_back()
            .map_or(0, |c| self.cursor - c.len_utf8())
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// Apply an editing key. Returns true if the text changed.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
                return true;
            }
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let start = before
                    .trim_end()
                    .rfind(char::is_whitespace)
                    .map_or(0, |i| i + 1);
                self.text.drain(start..self.cursor);
                self.cursor = start;
                return true;
            }
            KeyCode::Char(c) if !ctrl => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                return true;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let start = self.prev_boundary();
                self.text.drain(start..self.cursor);
                self.cursor = start;
                return true;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let end = self.next_boundary();
                self.text.drain(self.cursor..end);
                return true;
            }
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn quote_roundtrips_through_tokenize() {
        for name in ["web1", "my dir", "it's", "a\"b", "back\\slash", ""] {
            let line = format!("show {}", quote(name));
            assert_eq!(commands::tokenize(&line).unwrap(), ["show", name], "{line}");
        }
        assert_eq!(quote("web1"), "web1");
    }

    #[test]
    fn path_helpers() {
        assert_eq!(parent("servers/prod/db"), "servers/prod");
        assert_eq!(parent("servers"), "");
        assert_eq!(basename("servers/prod/db"), "db");
        assert_eq!(basename("servers"), "servers");
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("x/y", "a"), "x/y/a");
    }

    #[test]
    fn input_editing() {
        let mut input = Input::new("mv a b".into());
        assert!(input.handle(ctrl('w')));
        assert_eq!(input.text, "mv a ");
        input.handle(key(KeyCode::Char('é')));
        assert_eq!(input.text, "mv a é");
        input.handle(key(KeyCode::Left));
        input.handle(key(KeyCode::Backspace));
        assert_eq!(input.text, "mv aé");
        assert_eq!(input.cursor_col(), 4);
        input.handle(key(KeyCode::Delete));
        assert_eq!(input.text, "mv a");
        input.handle(ctrl('a'));
        input.handle(key(KeyCode::Char('x')));
        assert_eq!(input.text, "xmv a");
        input.handle(key(KeyCode::Right));
        assert!(input.handle(ctrl('u')));
        assert_eq!(input.text, "v a");
    }
}
