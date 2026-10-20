use super::fuzzy;
use super::generate::{self, Field, Form};
use super::history::{self, History};
use super::palette;
use super::sync::{Op, Sync};
use crate::commands::{self, COMMANDS, Session};
use crate::context::{self, Context};
use crate::crypto::{self, WrongPassphrase};
use crate::password::Spec;
use crate::{clipboard, fileio, git, index, store};
use age::secrecy::SecretString;
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How many recently used entries the bottom pane keeps (and `1`-`5` copy).
pub const RECENT_MAX: usize = 5;

/// Work the event loop must do: decrypting (which may first need the
/// identity unlocked), or anything with the terminal in hand ($EDITOR,
/// prompts, output).
pub enum Effect {
    None,
    /// Decrypt an entry into the view popup.
    Decrypt(String),
    /// Copy an entry's first line to the clipboard.
    Copy(String),
    /// Try the passphrase typed into the unlock popup.
    Unlock,
    /// Run commands with the terminal handed back to them.
    Run(Batch),
    /// `git pull` with the terminal handed over, so prompts (ssh
    /// passphrase, host key) can be answered; normally pulls run in the
    /// background behind a progress popup (`Mode::Sync`).
    Pull,
    /// `git push` with the terminal handed over, then quit.
    Push,
    /// Generate a password straight to the clipboard (the `g` form).
    Generate {
        path: String,
        spec: Spec,
        length: u32,
        in_place: bool,
    },
}

impl Effect {
    /// Whether it decrypts, and so needs the identity unlocked first.
    fn needs_keys(&self) -> bool {
        match self {
            // A new password opens in the view popup afterwards, so ask
            // before generating, not after.
            Effect::Decrypt(_) | Effect::Copy(_) | Effect::Generate { .. } => true,
            Effect::Run(batch) => batch.jobs.iter().any(|j| commands::needs_keys(&j.argv)),
            _ => false,
        }
    }
}

/// Asking for the identity's passphrase, to then carry out `then`.
pub struct Unlock {
    pub input: Input,
    pub error: Option<String>,
    /// The passphrase is being tried (it takes about a second).
    pub working: bool,
    then: Box<Effect>,
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
    /// The context the paths are in; it survives a switch, so `p` in
    /// another context moves or copies them across.
    pub context: Context,
}

impl Buffer {
    /// Whether the paths are in another context than `here`.
    pub fn elsewhere(&self, here: &Context) -> bool {
        self.context.name != here.name
    }
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

/// The `C` popup: every context, to switch to or add to.
pub struct Picker {
    pub contexts: Vec<Context>,
    pub list: ListState,
    /// Typing a new context: its name, then optionally an existing store's
    /// path.
    pub adding: Option<Input>,
    pub error: Option<String>,
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
    /// The `g` form.
    Generate(Box<Form>),
    /// A background `git pull`/`push` and its progress popup.
    Sync(Sync),
    /// The store's git history (`u`).
    History(Box<History>),
    /// Asking for the identity's passphrase.
    Unlock(Unlock),
    /// Picking a context (`C`).
    Contexts(Picker),
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
    /// Printed once the TUI has closed (e.g. a failed push).
    pub exit_warning: Option<String>,
    history: Vec<String>,
    history_pos: Option<usize>,
    /// The highlighted command-palette suggestion.
    pub palette_sel: usize,
    /// The context to switch to once the push it waits on is done (or
    /// declined), instead of quitting.
    pub switch_to: Option<Context>,
    /// Work to do on the next turn of the event loop (a new context's
    /// `init`), whichever way it was queued.
    pub next: Option<Effect>,
    /// Name the context in the header: there is more than one.
    pub show_context: bool,
}

impl App {
    pub fn new(sess: Session) -> Self {
        let clip_time = clipboard::clear_after();
        let recent = load_recent(&sess);
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
            exit_warning: None,
            history: Vec::new(),
            history_pos: None,
            palette_sel: 0,
            switch_to: None,
            next: None,
            show_context: false,
        };
        app.count_contexts();
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

    /// Whether a folder's contents show. The root row (`""`, named after
    /// the context) is always open.
    pub fn is_expanded(&self, path: &str) -> bool {
        path.is_empty() || self.filtering() || self.expanded.contains(path)
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
        // The store itself heads the tree, so new things, pastes and the
        // rest can target its top level like any folder.
        let mut rows = vec![Row {
            path: String::new(),
            name: self.sess.context.name.clone(),
            depth: 0,
            is_dir: true,
            context_only: self.filtering(),
        }];
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
                            depth: path.matches('/').count() + 1,
                            is_dir: *is_dir,
                            context_only: !matched.contains(path.as_str()),
                            path: path.clone(),
                        });
                    }
                }
            }
            _ => tree_rows(&self.sess.store, &self.expanded, "", 1, &mut rows),
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
        self.upstream = git::upstream(self.sess.store.root());
        self.ahead = if self.upstream.is_some() {
            git::ahead_count(self.sess.store.root())
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
        if let Some(buf) = &mut self.buffer
            && !buf.elsewhere(&self.sess.context)
        {
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
        if let Some(buf) = &mut self.buffer
            && !buf.elsewhere(&self.sess.context)
        {
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

    /// Persist recents so `1`-`5` work across sessions, one list per
    /// context. Store paths only; the store doesn't encrypt names either.
    fn save_recent(&self) {
        if let Some(path) = context::state_file(&self.sess.context.name, "recent") {
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
        if let Mode::Sync(sync) = &mut self.mode
            && let Some(true) = sync.poll()
        {
            let op = sync.op;
            // git's last line says what happened ("Already up to date.").
            let summary = sync.output.last().cloned();
            self.mode = Mode::Browse;
            match op {
                Op::Pull => {
                    let what = match summary {
                        Some(s) => format!("pulled — {}", s.trim()),
                        None => "pulled".into(),
                    };
                    self.finish_sync(&what, Ok(()));
                }
                Op::Push => self.after_push(None),
            }
        }
    }

    /// Whether something on screen is animating and wants frequent redraws.
    pub fn busy(&self) -> bool {
        matches!(&self.mode, Mode::Sync(s) if !s.failed)
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

    /// Keys for the generate form: Tab and ↑↓ between fields, ←→ (or
    /// space) through the types, digits or ←→ +- for the length, space
    /// (or ←→) to toggle look-alikes.
    fn generate_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Generate(form) = &mut self.mode else {
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let state = |f: &Form| {
            (
                f.name.text.clone(),
                f.kind,
                f.length.text.clone(),
                f.no_look_alikes,
            )
        };
        let before = state(form);
        match (form.field, key.code) {
            (_, KeyCode::Esc) => {
                self.mode = Mode::Browse;
                return Effect::None;
            }
            (_, KeyCode::Char('c')) if ctrl => {
                self.mode = Mode::Browse;
                return Effect::None;
            }
            (_, KeyCode::Enter) => return self.submit_generate(),
            (_, KeyCode::Tab | KeyCode::Down) => form.next_field(false),
            (_, KeyCode::BackTab | KeyCode::Up) => form.next_field(true),
            (Field::Name, _) => {
                form.name.handle(key);
            }
            (Field::Kind, KeyCode::Left | KeyCode::Char('h')) => form.cycle_kind(true),
            (Field::Kind, KeyCode::Right | KeyCode::Char('l' | ' ')) => form.cycle_kind(false),
            (Field::Length, KeyCode::Left | KeyCode::Char('-')) => form.nudge_length(-1),
            (Field::Length, KeyCode::Right | KeyCode::Char('+' | '=')) => form.nudge_length(1),
            (Field::Length, KeyCode::Char(c)) if !c.is_ascii_digit() => {}
            (Field::Length, _) => {
                form.length.handle(key);
            }
            (
                Field::LookAlikes,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ' | 'h' | 'l' | 'x'),
            ) => form.no_look_alikes = !form.no_look_alikes,
            _ => {}
        }
        // Any change takes back an "⏎ again to replace" warning.
        if before != state(form) {
            form.error = None;
            form.overwrite = false;
        }
        Effect::None
    }

    fn submit_generate(&mut self) -> Effect {
        let store = &self.sess.store;
        let Mode::Generate(form) = &mut self.mode else {
            return Effect::None;
        };
        let name = form.name.text.trim();
        if name.is_empty() {
            form.error = Some("give it a name".into());
            form.field = Field::Name;
            return Effect::None;
        }
        let path = store::resolve(&form.folder, name);
        if path.is_empty() || name.ends_with('/') || store.is_dir(&path) {
            form.error = Some(format!(
                "{} is a folder — add a name",
                store::display(&path)
            ));
            form.field = Field::Name;
            return Effect::None;
        }
        let Some(length) = form.length() else {
            form.error = Some(format!("length must be 1 to {}", generate::MAX_LENGTH));
            form.field = Field::Length;
            return Effect::None;
        };
        let in_place = store.is_entry(&path);
        if in_place && !form.overwrite {
            form.overwrite = true;
            form.error = Some(format!(
                "{} exists — ⏎ again replaces its password (other lines are kept)",
                store::display(&path)
            ));
            return Effect::None;
        }
        form.save_prefs();
        let spec = form.spec();
        self.mode = Mode::Browse;
        Effect::Generate {
            path,
            spec,
            length,
            in_place,
        }
    }

    /// The new password is on the clipboard: highlight it, start the
    /// countdown, and open it masked in the view popup.
    pub fn finish_generate(&mut self, path: String, result: Result<()>) -> Effect {
        self.reload_files();
        // Whatever was there, it now starts with a password.
        self.sess.files.remove(&path);
        self.sess.files.save(self.sess.store.root());
        if let Some(f) = &mut self.filter {
            f.index = self.sess.store.walk();
        }
        self.refresh_git();
        if let Err(e) = result {
            self.rebuild(None);
            self.error(format!("{e:#}"));
            return Effect::None;
        }
        self.go_to(&path);
        self.last_copy = Some((path.clone(), Instant::now()));
        self.push_recent(&path);
        Effect::Decrypt(path)
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
        // if it was wrong (e.g. after a change made elsewhere).
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
        // `:context` may have added, renamed or moved one.
        self.count_contexts();
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
            Mode::Generate(_) => self.generate_key(key),
            Mode::Sync(_) => self.sync_key(key),
            Mode::History(_) => self.history_key(key),
            Mode::Unlock(_) => self.unlock_key(key),
            Mode::Contexts(_) => self.contexts_key(key),
        }
    }

    /// Let `effect` through if it needs no keys or they are unlocked;
    /// otherwise unlock first (a plain key file needs no passphrase) and
    /// hold it until the passphrase is in.
    pub fn gate(&mut self, effect: Effect) -> Effect {
        if self.sess.keys.is_some() || !effect.needs_keys() {
            return effect;
        }
        let identity = match self.sess.context.identity() {
            Ok(path) => path,
            Err(e) => {
                self.error(format!("{e:#}"));
                return Effect::None;
            }
        };
        match crypto::needs_passphrase(&identity) {
            Ok(true) => {
                self.mode = Mode::Unlock(Unlock {
                    input: Input::default(),
                    error: None,
                    working: false,
                    then: Box::new(effect),
                });
                Effect::None
            }
            Ok(false) => match crypto::unlock(&identity, None) {
                Ok(keys) => {
                    self.sess.keys = Some(keys);
                    effect
                }
                Err(e) => {
                    self.error(format!("{e:#}"));
                    Effect::None
                }
            },
            Err(e) => {
                self.error(format!("{e:#}"));
                Effect::None
            }
        }
    }

    fn unlock_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Unlock(u) = &mut self.mode else {
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Char('c') if ctrl => self.mode = Mode::Browse,
            KeyCode::Enter if !u.input.text.is_empty() => {
                u.working = true;
                return Effect::Unlock;
            }
            _ => {
                if u.input.handle(key) {
                    u.error = None;
                }
            }
        }
        Effect::None
    }

    /// Try the typed passphrase: on success, carry on with what was held.
    pub fn try_unlock(&mut self) -> Effect {
        let Mode::Unlock(u) = &mut self.mode else {
            return Effect::None;
        };
        let passphrase = SecretString::from(std::mem::take(&mut u.input).text);
        let result = self
            .sess
            .context
            .identity()
            .and_then(|identity| crypto::unlock(&identity, Some(&passphrase)));
        let Mode::Unlock(u) = &mut self.mode else {
            return Effect::None;
        };
        match result {
            Ok(keys) => {
                self.sess.keys = Some(keys);
                let Mode::Unlock(u) = std::mem::replace(&mut self.mode, Mode::Browse) else {
                    unreachable!()
                };
                *u.then
            }
            Err(e) if e.is::<WrongPassphrase>() => {
                u.working = false;
                u.error = Some("wrong passphrase".into());
                Effect::None
            }
            Err(e) => {
                self.mode = Mode::Browse;
                self.error(format!("{e:#}"));
                Effect::None
            }
        }
    }

    fn open_history(&mut self) {
        if !self.sess.store.root().join(".git").exists() {
            return self.error("undo needs the store in git (`:git init`)");
        }
        match History::load(self.sess.store.root()) {
            Ok(h) if h.commits.is_empty() => self.error("the store has no history yet"),
            Ok(h) => self.mode = Mode::History(Box::new(h)),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Keys for the history view: move, `u` undo the highlighted commit,
    /// ⏎ roll the store back to it (after asking), esc back to the tree.
    fn history_key(&mut self, key: KeyEvent) -> Effect {
        let root = self.sess.store.root().to_path_buf();
        let Mode::History(h) = &mut self.mode else {
            return Effect::None;
        };
        self.status = None;
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if h.confirm {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    h.confirm = false;
                    self.rollback();
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => h.confirm = false,
                _ if ctrl_c => h.confirm = false,
                _ => {}
            }
            return Effect::None;
        }
        let page = self.page.max(1) as isize;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => h.move_by(1, &root),
            KeyCode::Char('k') | KeyCode::Up => h.move_by(-1, &root),
            KeyCode::PageDown => h.move_by(page, &root),
            KeyCode::PageUp => h.move_by(-page, &root),
            KeyCode::Home | KeyCode::Char('g') => h.select(0, &root),
            KeyCode::End | KeyCode::Char('G') => h.select(usize::MAX, &root),
            KeyCode::Char('u') => self.undo_commit(),
            KeyCode::Enter => {
                if h.list.selected() == Some(0) {
                    self.info("that is the newest change — pick an older one to roll back to");
                } else {
                    h.confirm = true;
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                self.leave_history()
            }
            _ if ctrl_c => self.leave_history(),
            _ => {}
        }
        Effect::None
    }

    /// Undo the highlighted commit with a new one, which then sits at the
    /// top of the list, so `u` again redoes it.
    fn undo_commit(&mut self) {
        let Mode::History(h) = &self.mode else {
            return;
        };
        let Some(commit) = h.selected() else {
            return;
        };
        if commit.parents > 1 {
            return self.error("a merge can't be undone on its own — ⏎ rolls back to before it");
        }
        if commit.parents == 0 {
            return self
                .error("that is the store's first commit — nothing before it to go back to");
        }
        let message = history::undo_message(&commit.subject);
        let touched = h.touched();
        let result = git::undo(self.sess.store.root(), &commit.hash, &message);
        self.after_history_change(&message, touched, result);
    }

    /// Put the store back as it was at the highlighted commit.
    fn rollback(&mut self) {
        let Mode::History(h) = &self.mode else {
            return;
        };
        let Some(commit) = h.selected() else {
            return;
        };
        let message = format!("Roll back to \"{}\" ({}).", commit.subject, commit.short);
        // Everything newer may come back or go away; any of it will do.
        let touched: Vec<String> = h.touched();
        let result = git::restore(self.sess.store.root(), &commit.hash, &message);
        self.after_history_change(&message, touched, result);
    }

    fn after_history_change(&mut self, message: &str, touched: Vec<String>, result: Result<()>) {
        if let Err(e) = result {
            return self.error(format!("{e:#}"));
        }
        self.store_changed();
        let root = self.sess.store.root().to_path_buf();
        let store = &self.sess.store;
        let focus = touched
            .into_iter()
            .find(|p| store.is_entry(p) || store.is_dir(p));
        if let Mode::History(h) = &mut self.mode {
            if focus.is_some() {
                h.focus = focus;
            }
            if let Err(e) = h.reload(&root) {
                return self.error(format!("{e:#}"));
            }
        }
        self.info(format!(
            "{} — u on it to reverse",
            message.trim_end_matches('.')
        ));
    }

    fn leave_history(&mut self) {
        let Mode::History(h) = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return;
        };
        if let Some(path) = h.focus {
            self.go_to(&path);
        }
    }

    /// Catch up with files changed under us (an undo, a rollback).
    fn store_changed(&mut self) {
        self.reload_files();
        let store = &self.sess.store;
        self.sess.files.prune(|p| store.is_entry(p));
        self.sess.files.save(store.root());
        self.prune();
        if let Some(f) = &mut self.filter {
            f.index = self.sess.store.walk();
        }
        self.rebuild(None);
        self.refresh_git();
    }

    /// Keys for the git popup: ignored while git runs; once it has failed,
    /// retry with the terminal handed over, or give up.
    fn sync_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Sync(sync) = &self.mode else {
            return Effect::None;
        };
        if !sync.failed {
            return Effect::None;
        }
        let op = sync.op;
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match (op, key.code) {
            (_, KeyCode::Char('r') | KeyCode::Enter) => {
                self.mode = Mode::Browse;
                match op {
                    Op::Pull => Effect::Pull,
                    Op::Push => Effect::Push,
                }
            }
            (Op::Pull, KeyCode::Esc | KeyCode::Char('q')) => {
                self.mode = Mode::Browse;
                self.finish_sync(
                    "",
                    Err(anyhow::anyhow!("pull failed (`:git pull` to retry)")),
                );
                Effect::None
            }
            (Op::Push, KeyCode::Char('q')) => {
                self.mode = Mode::Browse;
                self.after_push(Some(anyhow::anyhow!("git push failed")));
                Effect::None
            }
            (Op::Push, KeyCode::Esc) => {
                self.mode = Mode::Browse;
                self.switch_to = None;
                self.refresh_git();
                self.error("not pushed — q to try again, or `:git push`");
                Effect::None
            }
            _ if ctrl_c => {
                self.mode = Mode::Browse;
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Whether the header should name the context.
    fn count_contexts(&mut self) {
        self.show_context = self.sess.context.name != context::DEFAULT
            || context::load().is_ok_and(|c| c.len() > 1);
    }

    /// Once a push is done, failed (`error`) or declined: switch to the
    /// context waiting on it, or quit.
    pub fn after_push(&mut self, error: Option<anyhow::Error>) {
        let Some(target) = self.switch_to.take() else {
            if let Some(e) = error {
                self.exit_warning = Some(format!("warning: {e:#} — your commits are still local"));
            }
            self.quit = true;
            return;
        };
        let left = self.sess.context.name.clone();
        self.switch(target);
        if let Some(e) = error {
            self.error(format!("{e:#} — your commits in {left} are still local"));
        }
    }

    fn open_contexts(&mut self) {
        match context::load() {
            Ok(contexts) => {
                let mut list = ListState::default();
                let here = contexts
                    .iter()
                    .position(|c| c.name == self.sess.context.name)
                    .unwrap_or(0);
                list.select(Some(here));
                self.mode = Mode::Contexts(Picker {
                    contexts,
                    list,
                    adding: None,
                    error: None,
                });
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Keys for the context picker: move, ⏎ switch, `n` add a context,
    /// esc close. While adding: type `name` (or `name path`), ⏎ adds it.
    fn contexts_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Contexts(picker) = &mut self.mode else {
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(input) = &mut picker.adding {
            match key.code {
                KeyCode::Esc => picker.adding = None,
                KeyCode::Char('c') if ctrl => picker.adding = None,
                KeyCode::Enter => self.add_context(),
                _ => {
                    if input.handle(key) {
                        picker.error = None;
                    }
                }
            }
            return Effect::None;
        }
        let count = picker.contexts.len();
        let at = picker.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => picker.list.select(Some((at + 1).min(count - 1))),
            KeyCode::Char('k') | KeyCode::Up => picker.list.select(Some(at.saturating_sub(1))),
            KeyCode::Home | KeyCode::Char('g') => picker.list.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => picker.list.select(Some(count - 1)),
            KeyCode::Char('n') => {
                picker.adding = Some(Input::default());
                picker.error = None;
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                let target = picker.contexts[at].clone();
                self.request_switch(target);
            }
            KeyCode::Esc | KeyCode::Char('q' | 'C') => self.mode = Mode::Browse,
            KeyCode::Char('c') if ctrl => self.mode = Mode::Browse,
            _ => {}
        }
        Effect::None
    }

    /// Add the context typed into the picker and switch to it: `name` makes
    /// a new store in `$XDG_DATA_HOME/ezer-pass/<name>`; `name path` uses
    /// the store at `path`, making it if it isn't there.
    fn add_context(&mut self) {
        let Mode::Contexts(picker) = &mut self.mode else {
            return;
        };
        let text = picker.adding.as_ref().map_or("", |i| i.text.trim());
        if text.is_empty() {
            picker.adding = None;
            return;
        }
        let (name, path) = match text.split_once(char::is_whitespace) {
            Some((name, path)) => (name, Some(path.trim())),
            None => (text, None),
        };
        let result = path
            .map(context::user_path)
            .transpose()
            .and_then(|path| context::create(name, path));
        match result {
            Ok(ctx) => self.request_switch(ctx),
            Err(e) => picker.error = Some(format!("{e:#}")),
        }
    }

    /// Switch to `target`, first offering to push what this store hasn't.
    fn request_switch(&mut self, target: Context) {
        if target.name == self.sess.context.name {
            self.mode = Mode::Browse;
            return;
        }
        if self.upstream.is_some() {
            self.refresh_git();
        }
        if self.upstream.is_some() && self.ahead > 0 {
            self.switch_to = Some(target);
            self.mode = Mode::Confirm(Confirm::Push { ahead: self.ahead });
        } else {
            self.switch(target);
        }
    }

    /// Open `target`'s store in place of this one. The identity stays
    /// unlocked if the two share it; marks and the filter are dropped, since
    /// their paths belong to the old store. The buffer stays, to paste here.
    fn switch(&mut self, target: Context) {
        self.mode = Mode::Browse;
        if let Err(e) = self.sess.switch(target) {
            return self.error(format!("{e:#}"));
        }
        let name = self.sess.context.name.clone();
        self.recent = load_recent(&self.sess);
        self.last_copy = None;
        self.expanded.clear();
        self.marks.clear();
        self.filter = None;
        self.count_contexts();
        self.refresh_git();
        self.rebuild(None);
        self.select(0);
        if self.sess.store.uses_gpg() {
            return self.error(format!(
                "{name} is still encrypted with gpg — run `ezer-pass -C {name} migrate`"
            ));
        }
        if !self.sess.store.root().join(store::RECIPIENTS).exists() {
            // A new store: set who it is encrypted to (your own key).
            self.next = Some(Effect::Run(Batch::new(
                vec![Job::new(vec!["init".into()])],
                Pause::OnError,
            )));
            return self.info(format!(
                "switched to {name}, a new store encrypted to your key"
            ));
        }
        if self.upstream.is_some() {
            self.mode = Mode::Confirm(Confirm::Pull);
        }
        self.info(format!("switched to {name}"));
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
                    self.expanded.remove(&section);
                    self.rebuild(Some(section));
                }
            }
            KeyCode::Enter => match row {
                Some((path, true, _)) if self.filtering() => self.go_to_dir(&path),
                // The root stays open.
                Some((path, true, _)) if path.is_empty() => {}
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
                if let Some((path, _, _)) = row
                    && !path.is_empty()
                {
                    if !self.marks.remove(&path) {
                        self.marks.insert(path);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('x') => self.fill_buffer(true),
            KeyCode::Char('y') => self.fill_buffer(false),
            KeyCode::Char('p') => return self.paste(self.context_dir()),
            // On the root, renaming is the context's.
            KeyCode::Char('r') if row.as_ref().is_some_and(|r| r.0.is_empty()) => {
                let name = self.sess.context.name.clone();
                if name == context::DEFAULT {
                    self.error("default can't be renamed");
                } else {
                    self.prefill(format!("context rename {name} "));
                }
            }
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
            KeyCode::Char('g') => {
                self.mode = Mode::Generate(Box::new(Form::new(self.context_dir())));
            }
            KeyCode::Char('i') => self.prefill("insert ".into()),
            KeyCode::Char('n') => self.prefill("mkdir ".into()),
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
            KeyCode::Char('C') => self.open_contexts(),
            KeyCode::Char('u') => self.open_history(),
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
    fn targets(&mut self) -> Vec<String> {
        if self.marks.is_empty() {
            if self.selected_path().is_some_and(|p| p.is_empty()) {
                self.error("that is the whole store — pick what's in it");
                return Vec::new();
            }
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
        self.buffer = Some(Buffer {
            cut,
            paths,
            context: self.sess.context.clone(),
        });
        self.info(format!(
            "{} {n} item{} — p pastes into the highlighted folder, here or in another context (C)",
            if cut { "cut" } else { "copied" },
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Plan moving (cut) or copying the buffer into `target`, the context
    /// folder. Refuses the whole paste on any hard conflict; overwriting
    /// existing entries asks first.
    fn paste(&mut self, target: String) -> Effect {
        let Some(buf) = &self.buffer else {
            self.error("nothing to paste (x cuts, y copies)");
            return Effect::None;
        };
        if buf.elsewhere(&self.sess.context) {
            return self.paste_across(target);
        }
        let store = &self.sess.store;
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

    /// Paste a buffer filled in another context: `mv`/`cp` jobs naming the
    /// source as `context:/path`, which commit in both stores.
    fn paste_across(&mut self, target: String) -> Effect {
        let Some(buf) = &self.buffer else {
            return Effect::None;
        };
        let from = buf.context.name.clone();
        let source = match store::Store::open(buf.context.path.clone()) {
            Ok(store) => store,
            Err(e) => {
                self.error(format!("{e:#}"));
                return Effect::None;
            }
        };
        let store = &self.sess.store;
        let cut = buf.cut;
        let op = if cut { "mv" } else { "cp" };
        let mut jobs = Vec::new();
        let mut dests = HashSet::new();
        let mut conflicts = 0;
        let mut select = None;
        for src in &buf.paths {
            let is_dir = source.is_dir(src);
            let problem = if !is_dir && !source.is_entry(src) {
                Some(format!("{from}:{} no longer exists", store::display(src)))
            } else {
                None
            };
            let dest = join(&target, basename(src));
            let problem = problem.or_else(|| {
                if store.is_dir(&dest) || (is_dir && store.is_entry(&dest)) {
                    Some(format!("{} already exists", store::display(&dest)))
                } else if !dests.insert(dest.clone()) {
                    Some(format!("two items would land at {}", store::display(&dest)))
                } else {
                    None
                }
            });
            if let Some(msg) = problem {
                self.error(msg);
                return Effect::None;
            }
            if store.is_entry(&dest) {
                conflicts += 1;
            }
            select.get_or_insert_with(|| dest.clone());
            jobs.push(Job::new(vec![
                op.into(),
                "-f".into(),
                format!("{from}:{}", store::display(src)),
                store::display(&dest),
            ]));
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
        self.palette_sel = 0;
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
                let palette = palette::suggest(&input.text, &self.sess.store, &self.sess.cwd);
                // A command name that's still being typed (or needs
                // arguments) completes instead of running.
                let name = input.text.trim();
                let complete = !input.text.contains(char::is_whitespace)
                    && !palette.items.is_empty()
                    && palette::find(name).is_none_or(|s| s.needs_args());
                if complete {
                    let i = self.palette_sel.min(palette.items.len() - 1);
                    *input = Input::new(palette.items[i].line.clone());
                    self.palette_sel = 0;
                    return Effect::None;
                }
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
            KeyCode::Tab | KeyCode::Up | KeyCode::Down | KeyCode::BackTab
                if !palette::suggest(&input.text, &self.sess.store, &self.sess.cwd)
                    .items
                    .is_empty() =>
            {
                let items = palette::suggest(&input.text, &self.sess.store, &self.sess.cwd).items;
                let n = items.len();
                let sel = self.palette_sel.min(n - 1);
                match key.code {
                    KeyCode::Tab => {
                        *input = Input::new(items[sel].line.clone());
                        self.palette_sel = 0;
                    }
                    KeyCode::Up | KeyCode::BackTab => self.palette_sel = (sel + n - 1) % n,
                    _ => self.palette_sel = (sel + 1) % n,
                }
            }
            // History moved to Ctrl-P/Ctrl-N, with ↑↓ when nothing is
            // suggested.
            KeyCode::Up | KeyCode::Char('p')
                if !self.history.is_empty() && (key.code == KeyCode::Up || ctrl) =>
            {
                let pos = self
                    .history_pos
                    .map_or(self.history.len() - 1, |p| p.saturating_sub(1));
                self.history_pos = Some(pos);
                *input = Input::new(self.history[pos].clone());
            }
            KeyCode::Down | KeyCode::Char('n')
                if self.history_pos.is_some() && (key.code == KeyCode::Down || ctrl) =>
            {
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
                if input.handle(key) {
                    self.palette_sel = 0;
                }
            }
        }
        Effect::None
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Confirm(confirm) = &self.mode else {
            return Effect::None;
        };
        let default_yes = matches!(confirm, Confirm::Pull | Confirm::Push { .. });
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        // Backing out of a switch stays here, where `n` switches unpushed.
        if matches!(confirm, Confirm::Push { .. })
            && self.switch_to.is_some()
            && (key.code == KeyCode::Esc || ctrl_c)
        {
            self.switch_to = None;
            self.mode = Mode::Browse;
            return Effect::None;
        }
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
            (Confirm::Pull, true) => {
                self.mode = Mode::Sync(Sync::start(Op::Pull, self.sess.store.root()));
                Effect::None
            }
            (Confirm::Pull, false) => {
                self.info("not pulled (pull later with `:git pull`)");
                Effect::None
            }
            (Confirm::Push { .. }, true) => {
                self.mode = Mode::Sync(Sync::start(Op::Push, self.sess.store.root()));
                Effect::None
            }
            (Confirm::Push { .. }, false) => {
                self.after_push(None);
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
                    self.go_to("");
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
            // Switch here, offering a push first, rather than in `commands`.
            "context" if positional.first() == Some(&"use") && positional.len() == 2 => {
                match context::find(positional[1]) {
                    Ok(target) => self.request_switch(target),
                    Err(e) => self.error(format!("{e:#}")),
                }
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

/// The context's recent entries that still exist.
fn load_recent(sess: &Session) -> Vec<String> {
    context::state_file(&sess.context.name, "recent")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            s.lines()
                .filter(|p| sess.store.is_entry(p))
                .take(RECENT_MAX)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

pub fn join(dir: &str, name: &str) -> String {
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
