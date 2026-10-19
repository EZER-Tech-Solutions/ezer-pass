use super::fuzzy;
use crate::commands::{self, COMMANDS, Session};
use crate::store::{self, Node};
use crate::{fileio, pass};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use std::time::{Duration, Instant};

/// Work the event loop must do with the terminal in hand, because pass may
/// need it (pinentry, $EDITOR, prompts) or print to it.
pub enum Effect {
    None,
    /// Decrypt an entry into the preview pane, optionally revealing it.
    Decrypt {
        path: String,
        reveal: bool,
    },
    /// `pass show -c`: copy an entry's first line to the clipboard.
    Copy(String),
    /// Run a command-line command with the terminal handed back to it.
    Run {
        argv: Vec<String>,
        pause: Pause,
    },
    Pull,
    /// Push, then quit.
    Push,
}

/// Whether to hold the screen after a `Run` so its output can be read.
pub enum Pause {
    Always,
    OnError,
}

pub enum Secret {
    Text(String),
    File { filename: String, size: usize },
}

/// Decrypted contents of the selected entry. Dropped as soon as the
/// selection moves, so at most one plaintext is held at a time.
pub struct Decrypted {
    pub path: String,
    pub secret: Secret,
    /// When the first line was unmasked; it re-masks after `clip_time`.
    pub revealed_at: Option<Instant>,
}

pub struct Search {
    pub input: Input,
    /// Every store path (and whether it is a directory), walked on open.
    index: Vec<(String, bool)>,
    pub results: Vec<(String, bool)>,
    pub list: ListState,
}

impl Search {
    /// Size of the whole index, for the "matches/total" count.
    pub fn total(&self) -> usize {
        self.index.len()
    }
}

pub enum Confirm {
    Delete { path: String, is_dir: bool },
    Pull,
    Push { ahead: u64 },
}

pub enum Mode {
    Browse,
    Search(Search),
    Command(Input),
    Confirm(Confirm),
    Help,
}

pub struct Status {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub sess: Session,
    /// Contents of the current directory.
    pub nodes: Vec<Node>,
    pub list: ListState,
    pub decrypted: Option<Decrypted>,
    pub mode: Mode,
    pub status: Option<Status>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub quit: bool,
    pub clip_time: Duration,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl App {
    pub fn new(sess: Session) -> Self {
        let clip_time = std::env::var("PASSWORD_STORE_CLIP_TIME")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(45);
        let mut app = App {
            sess,
            nodes: Vec::new(),
            list: ListState::default(),
            decrypted: None,
            mode: Mode::Browse,
            status: None,
            upstream: None,
            ahead: 0,
            quit: false,
            clip_time: Duration::from_secs(clip_time),
            history: Vec::new(),
            history_pos: None,
        };
        app.refresh_git();
        if app.upstream.is_some() {
            app.mode = Mode::Confirm(Confirm::Pull);
        }
        app.reload(None);
        app
    }

    pub fn selected(&self) -> Option<&Node> {
        self.list.selected().and_then(|i| self.nodes.get(i))
    }

    /// Store path of the selected node.
    pub fn selected_path(&self) -> Option<String> {
        self.selected().map(|n| self.child_path(&n.name))
    }

    fn child_path(&self, name: &str) -> String {
        if self.sess.cwd.is_empty() {
            name.to_string()
        } else {
            format!("{}/{name}", self.sess.cwd)
        }
    }

    /// The decrypted secret, if it belongs to the selected entry.
    pub fn current_secret(&self) -> Option<&Decrypted> {
        let path = self.selected_path()?;
        self.decrypted.as_ref().filter(|d| d.path == path)
    }

    /// Re-read the current directory, selecting `select` by name if given,
    /// else keeping the current selection's name (or index). Climbs to the
    /// nearest existing ancestor if the directory itself was removed.
    pub fn reload(&mut self, select: Option<&str>) {
        let keep = select
            .map(str::to_string)
            .or_else(|| self.selected().map(|n| n.name.clone()));
        let old_index = self.list.selected().unwrap_or(0);
        loop {
            match self.sess.store.list(&self.sess.cwd) {
                Ok(nodes) => {
                    self.nodes = nodes;
                    break;
                }
                Err(_) if !self.sess.cwd.is_empty() => {
                    self.sess.cwd = store::resolve(&self.sess.cwd, "..");
                }
                Err(e) => {
                    self.nodes.clear();
                    self.error(format!("{e:#}"));
                    break;
                }
            }
        }
        let index = keep
            .and_then(|name| self.nodes.iter().position(|n| n.name == name))
            .unwrap_or(old_index.min(self.nodes.len().saturating_sub(1)));
        self.select(index);
    }

    pub fn refresh_git(&mut self) {
        self.upstream = pass::git_upstream(self.sess.store.root());
        self.ahead = if self.upstream.is_some() {
            pass::git_ahead_count(self.sess.store.root())
        } else {
            0
        };
    }

    fn select(&mut self, index: usize) {
        if self.nodes.is_empty() {
            self.list.select(None);
        } else {
            self.list.select(Some(index.min(self.nodes.len() - 1)));
        }
        if self.current_secret().is_none() {
            self.decrypted = None;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let index = self.list.selected().unwrap_or(0);
        self.select(index.saturating_add_signed(delta));
    }

    fn cd(&mut self, path: String, select: Option<&str>) {
        self.sess.cwd = path;
        self.list.select(Some(0));
        self.reload(select);
    }

    /// Go to a store path: into a directory, or to an entry's directory with
    /// the entry selected.
    fn navigate_to(&mut self, path: &str) {
        if self.sess.store.is_dir(path) {
            self.cd(path.to_string(), None);
        } else {
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            self.cd(parent.to_string(), Some(name));
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

    /// Re-mask a revealed secret once the clipboard timeout has passed.
    pub fn tick(&mut self) {
        if let Some(d) = &mut self.decrypted
            && d.revealed_at.is_some_and(|t| t.elapsed() >= self.clip_time)
        {
            d.revealed_at = None;
        }
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

    pub fn finish_decrypt(&mut self, path: String, result: Result<String>, reveal: bool) {
        match result {
            Ok(content) => {
                let secret = match fileio::decode_payload(&content) {
                    Ok(file) => Secret::File {
                        filename: file.filename,
                        size: file.data.len(),
                    },
                    Err(_) if fileio::marker_filename(&content).is_some() => {
                        self.error("file entry is corrupt");
                        return;
                    }
                    Err(_) => Secret::Text(content),
                };
                self.decrypted = Some(Decrypted {
                    path,
                    secret,
                    revealed_at: reveal.then(Instant::now),
                });
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    pub fn finish_copy(&mut self, path: &str, result: Result<()>) {
        match result {
            Ok(()) => self.info(format!(
                "copied {} to clipboard (clears in {}s)",
                store::display(path),
                self.clip_time.as_secs()
            )),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// After a command ran with the terminal: re-read everything it may
    /// have changed.
    pub fn finish_run(&mut self, result: Result<()>) {
        self.decrypted = None;
        self.reload(None);
        self.refresh_git();
        if let Err(e) = result {
            self.error(format!("{e:#}"));
        }
    }

    pub fn finish_sync(&mut self, what: &str, result: Result<()>) {
        self.reload(None);
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
            Mode::Search(_) => self.search_key(key),
            Mode::Command(_) => self.command_key(key),
            Mode::Confirm(_) => self.confirm_key(key),
            Mode::Help => {
                self.mode = Mode::Browse;
                Effect::None
            }
        }
    }

    fn browse_key(&mut self, key: KeyEvent) -> Effect {
        self.status = None;
        let sel = self.selected().cloned();
        let sel_path = self.selected_path();
        let sel_entry = sel.as_ref().filter(|n| !n.is_dir);
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => match (&sel, sel_path) {
                (Some(n), Some(path)) if n.is_dir => self.cd(path, None),
                (Some(_), Some(path)) if self.current_secret().is_none() => {
                    return Effect::Decrypt {
                        path,
                        reveal: false,
                    };
                }
                _ => {}
            },
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace
                if !self.sess.cwd.is_empty() =>
            {
                let (parent, name) = match self.sess.cwd.rsplit_once('/') {
                    Some((p, n)) => (p.to_string(), n.to_string()),
                    None => (String::new(), self.sess.cwd.clone()),
                };
                self.cd(parent, Some(&name));
            }
            KeyCode::Char(' ') if sel_entry.is_some() => {
                if let Some(d) = &mut self.decrypted
                    && Some(&d.path) == sel_path.as_ref()
                {
                    d.revealed_at = match d.revealed_at {
                        Some(_) => None,
                        None => Some(Instant::now()),
                    };
                } else if let Some(path) = sel_path {
                    return Effect::Decrypt { path, reveal: true };
                }
            }
            KeyCode::Char('c') if sel_entry.is_some() => {
                return Effect::Copy(sel_path.expect("entry selected"));
            }
            KeyCode::Char('e') if sel_entry.is_some() => {
                return Effect::Run {
                    argv: vec![
                        "edit".into(),
                        store::display(&sel_path.expect("entry selected")),
                    ],
                    pause: Pause::OnError,
                };
            }
            KeyCode::Char('d') => {
                if let (Some(n), Some(path)) = (&sel, sel_path) {
                    self.mode = Mode::Confirm(Confirm::Delete {
                        path,
                        is_dir: n.is_dir,
                    });
                }
            }
            KeyCode::Char('r') => {
                if let Some(n) = &sel {
                    let q = quote(&n.name);
                    self.prefill(format!("mv {q} {q}"));
                }
            }
            KeyCode::Char('x') => {
                if let Some(n) = sel_entry {
                    self.prefill(format!("export {} ", quote(&n.name)));
                }
            }
            KeyCode::Char('g') => self.prefill("generate ".into()),
            KeyCode::Char('i') => self.prefill("insert ".into()),
            KeyCode::Char('m') => self.prefill("mkdir ".into()),
            KeyCode::Char('I') => self.prefill("import ".into()),
            KeyCode::Char(':') => self.prefill(String::new()),
            KeyCode::Char('/') => {
                let index = self.sess.store.walk();
                let results = index.clone();
                let mut list = ListState::default();
                list.select((!results.is_empty()).then_some(0));
                self.mode = Mode::Search(Search {
                    input: Input::default(),
                    index,
                    results,
                    list,
                });
            }
            KeyCode::Char('R') => {
                self.reload(None);
                self.refresh_git();
                self.info("refreshed");
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Effect::None
    }

    fn prefill(&mut self, text: String) {
        self.history_pos = None;
        self.mode = Mode::Command(Input::new(text));
    }

    fn search_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Search(search) = &mut self.mode else {
            return Effect::None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = search.results.len();
        let cur = search.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Char('c') if ctrl => self.mode = Mode::Browse,
            KeyCode::Enter => {
                if let Some((path, _)) = search.list.selected().and_then(|i| search.results.get(i))
                {
                    let path = path.clone();
                    self.mode = Mode::Browse;
                    self.navigate_to(&path);
                }
            }
            KeyCode::Down | KeyCode::Tab => search
                .list
                .select(Some((cur + 1).min(len.saturating_sub(1)))),
            KeyCode::Char('n') if ctrl => search
                .list
                .select(Some((cur + 1).min(len.saturating_sub(1)))),
            KeyCode::Up | KeyCode::BackTab => search.list.select(Some(cur.saturating_sub(1))),
            KeyCode::Char('p') if ctrl => search.list.select(Some(cur.saturating_sub(1))),
            _ => {
                if search.input.handle(key) {
                    search.results = fuzzy::rank(&search.input.text, &search.index);
                    search
                        .list
                        .select((!search.results.is_empty()).then_some(0));
                }
            }
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
        let default_yes = !matches!(confirm, Confirm::Delete { .. });
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
            (Confirm::Delete { path, is_dir }, true) => {
                let mut argv = vec!["rm".to_string(), "-f".to_string()];
                if is_dir {
                    argv.push("-r".into());
                }
                argv.push(store::display(&path));
                Effect::Run {
                    argv,
                    pause: Pause::OnError,
                }
            }
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
            (Confirm::Delete { .. }, false) => Effect::None,
        }
    }

    /// Run a `:` command line. Navigation and anything that would print a
    /// secret are handled in the TUI; the rest go to `commands::run` with
    /// the terminal handed over.
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
        match cmd {
            "exit" | "quit" => self.request_quit(),
            "help" | "?" => self.mode = Mode::Help,
            "pwd" => self.info(store::display(&self.sess.cwd)),
            "cd" | "ls" | "list" if positional.len() <= 1 => {
                let default = if cmd == "cd" { "/" } else { "." };
                let path = self
                    .sess
                    .resolve(positional.first().copied().unwrap_or(default));
                if self.sess.store.is_dir(&path) || (cmd != "cd" && self.sess.store.is_entry(&path))
                {
                    self.navigate_to(&path);
                } else if self.sess.store.is_entry(&path) {
                    self.error(format!("{} is not a directory", store::display(&path)));
                } else {
                    self.error(format!(
                        "{} is not in the password store",
                        store::display(&path)
                    ));
                }
            }
            "mkdir" if positional.len() == 1 => {
                let path = self.sess.resolve(positional[0]);
                match self.sess.store.mkdir(&path) {
                    Ok(()) => {
                        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                        self.reload(Some(&name));
                        self.info(format!("created {}/", store::display(&path)));
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            // Show into the preview pane rather than printing plaintext.
            "show" if positional.len() == 1 => {
                let path = self.sess.resolve(positional[0]);
                if !self.sess.store.is_entry(&path) {
                    self.error(format!(
                        "{} is not in the password store",
                        store::display(&path)
                    ));
                    return Effect::None;
                }
                self.navigate_to(&path);
                return if flags.iter().any(|f| *f == "-c" || *f == "--clip") {
                    Effect::Copy(path)
                } else {
                    Effect::Decrypt { path, reveal: true }
                };
            }
            _ if argv.len() == 1 && self.sess.store.is_dir(&self.sess.resolve(cmd)) => {
                self.cd(self.sess.resolve(cmd), None);
            }
            _ if COMMANDS.contains(&cmd) => {
                // Quiet commands return straight to the TUI; anything that
                // prints (generate, find, git log, ...) holds for a keypress.
                let pause = match cmd {
                    "edit" | "insert" | "mv" | "cp" | "rm" => Pause::OnError,
                    _ => Pause::Always,
                };
                return Effect::Run { argv, pause };
            }
            _ => self.error(format!("unknown command {cmd:?} (`?` for help)")),
        }
        Effect::None
    }
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
